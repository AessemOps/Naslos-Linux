<script lang="ts">
  import { onDestroy } from 'svelte';
  import AsyncState from './AsyncState.svelte';
  import Button from './Button.svelte';
  import DataTable from './DataTable.svelte';
  import { confirmAction } from '$lib/stores/confirm';
  import { formatBytes } from '$lib/format';

  // ISO/disk-image library (FR-VMT): chunked browser upload with progress,
  // server-side URL import (the SSRF guard lives in vmd) and delete.
  export let pool: string;
  export let refreshKey: number = 0;

  let images: { name: string; sizeBytes: number }[] = [];
  let importing: string[] = [];
  let importErrors: Record<string, string> = {};
  let error = '';
  let loadError = '';
  let loading = false;

  let uploadFile: FileList | null = null;
  let uploadProgress = 0;
  let uploading = false;
  let deleting = '';
  // The last offset the server confirmed for the selected file, so retrying a
  // failed upload resumes instead of truncating and starting at zero.
  let uploadKey = '';
  let uploadOffset = 0;
  // One owner token per upload session, echoed on every chunk so vmd can
  // refuse a different client instead of discarding active progress. It is
  // mirrored into sessionStorage per pool+name so a panel remount (reload,
  // tab switch) resumes the same server session instead of minting a new
  // token that the live session would refuse.
  let uploadOwner = '';
  let ownerScope = '';
  let uploadGeneration = 0;
  let uploadXhr: XMLHttpRequest | null = null;
  let cancelling = false;
  // Replacing a library file needs an explicit acknowledgement (vmd requires
  // confirm=<name>); the checkbox appears only when the target name is already
  // in the loaded library.
  let replaceUpload = false;
  let uploadTarget = '';
  let uploadReplaces = false;

  let importUrl = '';
  let importName = '';
  let importSha = '';
  let importingUrl = false;
  let replaceImport = false;
  let importTarget = '';
  let importReplaces = false;

  const CHUNK = 8 * 1024 * 1024;

  function urlFileName(url: string): string {
    return url.split('/').pop()?.split('?')[0] || 'image.iso';
  }

  function newUploadOwner(): string {
    if (typeof crypto !== 'undefined' && typeof crypto.randomUUID === 'function') {
      return crypto.randomUUID();
    }
    return `upload-${Date.now()}-${Math.random().toString(36).slice(2, 12)}`;
  }

  function ownerStorageKey(poolName: string, name: string): string {
    return `naslos:upload-owner:${poolName}/${name}`;
  }

  function storedOwner(poolName: string, name: string): string {
    if (!name) return '';
    try {
      return sessionStorage.getItem(ownerStorageKey(poolName, name)) ?? '';
    } catch {
      // Blocked storage only costs reload persistence; the upload still works.
      return '';
    }
  }

  function rememberOwner(poolName: string, name: string, owner: string) {
    if (!name) return;
    try {
      if (owner) sessionStorage.setItem(ownerStorageKey(poolName, name), owner);
      else sessionStorage.removeItem(ownerStorageKey(poolName, name));
    } catch {
      // See storedOwner.
    }
  }

  // Restore the token whenever the selected target (or pool) changes, so the
  // cancel action is available before the first retry.
  $: {
    const scope = `${pool}/${uploadTarget}`;
    if (scope !== ownerScope) {
      ownerScope = scope;
      uploadOwner = uploadTarget ? storedOwner(pool, uploadTarget) : '';
    }
  }

  async function releaseUpload(name: string, owner: string): Promise<boolean> {
    if (!name || !owner) return false;
    try {
      const res = await fetch(
        `/api/virt/images/upload?pool=${encodeURIComponent(pool)}&name=${encodeURIComponent(name)}&owner=${encodeURIComponent(owner)}`,
        { method: 'DELETE' }
      );
      return res.ok;
    } catch {
      return false;
    }
  }

  async function cancelUpload() {
    if (cancelling) return;
    const name = uploadTarget || uploadFile?.[0]?.name || '';
    const owner = uploadOwner || storedOwner(pool, name);
    cancelling = true;
    // Invalidate every callback of the aborted transfer before the DELETE
    // releases the server session.
    uploadGeneration += 1;
    uploadXhr?.abort();
    uploadXhr = null;
    const released = await releaseUpload(name, owner);
    if (released) rememberOwner(pool, name, '');
    uploadOwner = '';
    uploading = false;
    uploadProgress = 0;
    uploadKey = '';
    uploadOffset = 0;
    cancelling = false;
    // Reload first: load() clears the error banner, and the operator should
    // still see the cancellation result.
    await load();
    error = released
      ? 'Upload cancelled; the staged file was removed.'
      : 'Upload cancelled locally; the server session will expire if it was not released.';
  }

  // The upload target is the selected file's name; the import target is the
  // explicit name or the URL's file name. Both are matched against the loaded
  // listing, which is what the operator sees.
  $: uploadTarget = uploadFile?.[0]?.name ?? '';
  $: uploadReplaces = uploadTarget !== '' && images.some((img) => img.name === uploadTarget);
  $: if (!uploadReplaces) replaceUpload = false;
  $: importTarget = importName.trim() || (importUrl.trim() ? urlFileName(importUrl.trim()) : '');
  $: importReplaces = importTarget !== '' && images.some((img) => img.name === importTarget);
  $: if (!importReplaces) replaceImport = false;

  $: pool, refreshKey, load();

  // A pool switch starts a new request while the previous one is still in
  // flight; a late answer for the old pool must never overwrite the new one.
  let loadSeq = 0;

  async function load() {
    const requested = pool;
    const seq = ++loadSeq;
    if (!requested) {
      images = [];
      importing = [];
      importErrors = {};
      return;
    }
    loading = true;
    loadError = '';
    try {
      const res = await fetch(`/api/virt/images?pool=${encodeURIComponent(requested)}`);
      const data = await res.json();
      if (seq !== loadSeq || requested !== pool) return;
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      images = Array.isArray(data.images) ? data.images : [];
      importing = Array.isArray(data.importing) ? data.importing : [];
      importErrors = data.errors ?? {};
    } catch (e: unknown) {
      if (seq !== loadSeq || requested !== pool) return;
      loadError = e instanceof Error ? e.message : String(e);
      images = [];
    } finally {
      if (seq === loadSeq) loading = false;
    }
  }

  // The repo's first browser file upload: XHR (progress events), chunked into
  // 8 MiB octet-stream requests the API relays to vmd.
  function upload() {
    const file = uploadFile?.[0];
    if (!file || uploading || cancelling) return;
    const name = file.name;
    const key = `${file.name}:${file.size}:${file.lastModified}`;
    const stored = storedOwner(pool, name);
    if (uploadKey !== key) {
      uploadKey = key;
      uploadOffset = 0;
      uploadOwner = stored || newUploadOwner();
      rememberOwner(pool, name, uploadOwner);
    } else if (!uploadOwner) {
      uploadOwner = stored || newUploadOwner();
      rememberOwner(pool, name, uploadOwner);
    }
    const owner = uploadOwner;
    const generation = ++uploadGeneration;
    let offset = uploadOffset;
    const total = file.size || 1;
    uploading = true;
    uploadProgress = Math.round((offset / total) * 100);
    error = '';
    // vmd requires the exact name as confirmation to overwrite an existing
    // image; a fresh upload does not need it.
    const confirmQuery = replaceUpload && name === uploadTarget
      ? `&confirm=${encodeURIComponent(name)}`
      : '';
    const xhr = new XMLHttpRequest();
    uploadXhr = xhr;
    const sendChunk = () => {
      const slice = file.slice(offset, offset + CHUNK);
      const final = offset + CHUNK >= file.size;
      xhr.open('POST', `/api/virt/images/upload?pool=${encodeURIComponent(pool)}&name=${encodeURIComponent(name)}&offset=${offset}&final=${final}${confirmQuery}&owner=${encodeURIComponent(owner)}`);
      xhr.setRequestHeader('Content-Type', 'application/octet-stream');
      xhr.upload.onprogress = (event) => {
        if (event.lengthComputable) {
          uploadProgress = Math.round(((offset + event.loaded) / total) * 100);
        }
      };
      xhr.onload = () => {
        if (generation !== uploadGeneration) return;
        if (uploadXhr === xhr) uploadXhr = null;
        if (xhr.status >= 200 && xhr.status < 300) {
          // Trust the size the server reports: it is the confirmed file length,
          // not just what the browser believes it sent.
          let confirmed = offset + slice.size;
          try {
            const data = JSON.parse(xhr.responseText);
            if (typeof data.sizeBytes === 'number' && data.sizeBytes >= confirmed) {
              confirmed = data.sizeBytes;
            }
          } catch {
            // Older vmd responses may be empty; the chunk size is still valid.
          }
          offset = confirmed;
          uploadOffset = confirmed;
          if (offset >= file.size) {
            uploading = false;
            uploadProgress = 100;
            uploadKey = '';
            uploadOffset = 0;
            rememberOwner(pool, name, '');
            uploadOwner = '';
            uploadFile = null;
            load();
          } else {
            sendChunk();
          }
        } else {
          uploading = false;
          let detail = `upload failed (HTTP ${xhr.status})`;
          try {
            detail = JSON.parse(xhr.responseText).error || detail;
          } catch {
            // Keep the HTTP fallback.
          }
          // vmd reports a lost/reset staging file as "start at offset 0" or
          // "restart from offset 0"; older releases said "past the end".
          // Reset the resume point so the retry is accepted instead of
          // repeating the same stale offset forever.
          if (/past the end|start at offset 0|restart from offset 0/i.test(detail)) {
            uploadKey = key;
            uploadOffset = 0;
            uploadProgress = 0;
            error = 'The server no longer has the partial upload; retry to start from the beginning.';
          } else {
            if (xhr.status === 413) {
              // Over the library cap: vmd removed the staged file, so a retry
              // must start over (the same file will still be too large).
              uploadKey = '';
              uploadOffset = 0;
              rememberOwner(pool, name, '');
              uploadOwner = '';
              uploadProgress = 0;
            }
            if (xhr.status === 409) {
              if (/different client/i.test(detail)) {
                // Another client owns the live session: release any session
                // this token still owns (best effort) and surface the reason.
                void releaseUpload(name, owner).then((released) => {
                  if (released) {
                    rememberOwner(pool, name, '');
                    uploadOwner = '';
                  }
                });
              }
              // The loaded listing was stale (the image already exists) or
              // another client owns the name: refresh so the replace checkbox
              // can appear, then keep the server's reason visible.
              load().finally(() => {
                if (generation === uploadGeneration) error = detail;
              });
            } else {
              error = detail;
            }
          }
        }
      };
      xhr.onerror = () => {
        if (generation !== uploadGeneration) return;
        if (uploadXhr === xhr) uploadXhr = null;
        uploading = false;
        error = 'The upload connection failed; retry to resume from the last confirmed chunk.';
      };
      xhr.send(slice);
    };
    sendChunk();
  }

  async function importFromUrl() {
    if (importingUrl || !importUrl.trim()) return;
    importingUrl = true;
    error = '';
    try {
      const name = importTarget;
      const res = await fetch('/api/virt/images/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          pool,
          name,
          url: importUrl.trim(),
          sha256: importSha.trim() || null,
          confirm: replaceImport && name === importTarget ? name : null
        })
      });
      const data = await res.json();
      if (!res.ok) {
        const message = data.error || `HTTP ${res.status}`;
        // A stale listing hides the replace checkbox; refresh it before the
        // catch displays the conflict.
        if (res.status === 409) await load();
        throw new Error(message);
      }
      importUrl = '';
      importName = '';
      importSha = '';
      replaceImport = false;
      // The import runs server-side; poll until it lands in the listing.
      pollImport(name);
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      importingUrl = false;
    }
  }

  // Import pollers: torn down with the component (closing the panel must not
  // leave a dead timer fetching the library for the whole import), and skipped
  // while the tab is hidden.
  let importTimers: ReturnType<typeof setInterval>[] = [];

  function pollImport(name: string) {
    const timer = setInterval(async () => {
      if (document.visibilityState !== 'visible') return;
      await load();
      if (!importing.includes(name)) clearInterval(timer);
    }, 3000);
    importTimers.push(timer);
  }

  onDestroy(() => {
    for (const timer of importTimers) clearInterval(timer);
    importTimers = [];
  });

  async function remove(name: string) {
    const confirmed = await confirmAction({
      title: 'Delete image',
      message: `Delete ${name} from the ${pool} library?`,
      confirmLabel: 'Delete',
      tone: 'danger'
    });
    if (!confirmed) return;
    deleting = name;
    error = '';
    try {
      const res = await fetch(`/api/virt/images/${encodeURIComponent(name)}?pool=${encodeURIComponent(pool)}`, {
        method: 'DELETE'
      });
      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `HTTP ${res.status}`);
      }
      await load();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      deleting = '';
    }
  }
</script>

<h2 class="text-lg font-bold mb-3">Images — {pool}/vms/isos</h2>

{#if error}
  <div role="alert" class="callout callout-bad">{error}</div>
{/if}

<form class="card mb-4" on:submit|preventDefault={upload}>
  <div class="flex flex-wrap items-end gap-3">
    <div>
      <label class="label" for="img-file">Upload a file (ISO or cloud image)</label>
      <input id="img-file" type="file" class="input" bind:files={uploadFile} disabled={uploading} />
    </div>
    <Button
      type="submit"
      loading={uploading}
      disabled={!uploadFile || cancelling || (uploadReplaces && !replaceUpload)}
    >
      {uploading ? `Uploading… ${uploadProgress}%` : 'Upload'}
    </Button>
    {#if uploading || (uploadTarget && (uploadOwner || storedOwner(pool, uploadTarget)))}
      <Button variant="secondary" loading={cancelling} on:click={cancelUpload}>
        {cancelling ? 'Cancelling…' : 'Cancel upload'}
      </Button>
    {/if}
    {#if uploading}
      <progress class="w-48" value={uploadProgress} max="100"></progress>
    {/if}
  </div>
  {#if uploadReplaces}
    <label class="mt-2 flex items-center gap-2 text-sm text-amber-300">
      <input id="img-replace" type="checkbox" class="w-4 h-4 rounded" bind:checked={replaceUpload} />
      <span>
        Replace the existing image <code class="font-mono text-xs">{uploadTarget}</code>: the current file is
        overwritten when the upload finishes. Stop every VM using it first.
      </span>
    </label>
  {/if}
  <p class="text-xs text-gray-500 mt-2">
    Uploaded in 8 MiB chunks; a failed chunk resumes from the last confirmed offset. Names are flat
    (letters, digits, '-', '_', '.').
  </p>
</form>

<form class="card mb-4" on:submit|preventDefault={importFromUrl}>
  <div class="grid grid-cols-1 gap-3 items-end md:grid-cols-4">
    <div class="md:col-span-2">
      <label class="label" for="img-url">Import from URL</label>
      <input id="img-url" class="input w-full" bind:value={importUrl}
        placeholder="https://cloud.debian.org/…/debian-13-netinst.iso" disabled={importingUrl} />
    </div>
    <div>
      <label class="label" for="img-name">Name (optional)</label>
      <input id="img-name" class="input w-full" bind:value={importName} placeholder="from the URL" disabled={importingUrl} />
    </div>
    <Button
      type="submit"
      loading={importingUrl}
      disabled={!importUrl.trim() || (importReplaces && !replaceImport)}
    >
      {importingUrl ? 'Starting…' : 'Import'}
    </Button>
  </div>
  {#if importReplaces}
    <label class="mt-2 flex items-center gap-2 text-sm text-amber-300">
      <input id="img-import-replace" type="checkbox" class="w-4 h-4 rounded" bind:checked={replaceImport} />
      <span>
        Replace the existing image <code class="font-mono text-xs">{importTarget}</code>: it is overwritten
        once the download completes. Stop every VM using it first.
      </span>
    </label>
  {/if}
  <div class="mt-2">
    <label class="label" for="img-sha">sha256 (optional, verified)</label>
    <input id="img-sha" class="input w-full max-w-md font-mono text-xs" bind:value={importSha}
      placeholder="64 hex chars; the download is deleted on mismatch" disabled={importingUrl} />
  </div>
  <p class="text-xs text-gray-500 mt-2">
    The node downloads the file itself (public http(s) targets only). Watch the list below for completion.
  </p>
</form>

{#if importing.length > 0}
  <div class="card mb-4 text-sm text-gray-300">
    Importing: {#each importing as n}<span class="text-naslos-link">{n}</span> {/each}
  </div>
{/if}
{#each Object.entries(importErrors) as [name, err]}
  <div role="alert" class="callout callout-bad">
    {name}: {err}
  </div>
{/each}

<AsyncState
  {loading}
  error={loadError}
  empty={!loading && !loadError && images.length === 0}
  loadingText="Loading library…"
  emptyTitle="The library is empty."
  emptyDetail="Upload an ISO or import one from a URL."
  emptyIcon="hard-drive"
  onRetry={load}
>
  <DataTable label="Image library">
    <table>
      <thead>
        <tr>
          <th scope="col">Name</th>
          <th scope="col">Size</th>
          <th scope="col"><span class="sr-only">Actions</span></th>
        </tr>
      </thead>
      <tbody>
        {#each images as img}
          <tr>
            <td class="font-mono text-xs">{img.name}</td>
            <td class="tabular-nums">{formatBytes(img.sizeBytes)}</td>
            <td>
              <div class="flex justify-end">
                <Button
                  variant="danger"
                  size="sm"
                  loading={deleting === img.name}
                  disabled={!!deleting}
                  on:click={() => remove(img.name)}
                >
                  Delete
                </Button>
              </div>
            </td>
          </tr>
        {/each}
      </tbody>
    </table>
  </DataTable>
</AsyncState>
