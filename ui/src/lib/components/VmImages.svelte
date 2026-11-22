<script lang="ts">
  import { onDestroy } from 'svelte';
  import { formatBytes } from '$lib/format';

  // ISO/disk-image library (FR-VMT): chunked browser upload with progress,
  // server-side URL import (the SSRF guard lives in vmd) and delete.
  export let pool: string;
  export let refreshKey: number = 0;

  let images: any[] = [];
  let importing: string[] = [];
  let importErrors: any = {};
  let error = '';
  let loading = false;

  let uploadFile: any = null;
  let uploadProgress = 0;
  let uploading = false;
  // The last offset the server confirmed for the selected file, so retrying a
  // failed upload resumes instead of truncating and starting at zero.
  let uploadKey = '';
  let uploadOffset = 0;

  let importUrl = '';
  let importName = '';
  let importSha = '';
  let importingUrl = false;

  const CHUNK = 8 * 1024 * 1024;

  $: pool, refreshKey, load();

  async function load() {
    if (!pool) return;
    loading = true;
    error = '';
    try {
      const res = await fetch(`/api/virt/images?pool=${encodeURIComponent(pool)}`);
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      images = Array.isArray(data.images) ? data.images : [];
      importing = Array.isArray(data.importing) ? data.importing : [];
      importErrors = data.errors ?? {};
    } catch (e: any) {
      error = e.message;
      images = [];
    } finally {
      loading = false;
    }
  }

  // The repo's first browser file upload: XHR (progress events), chunked into
  // 8 MiB octet-stream requests the API relays to vmd.
  function upload() {
    if (!uploadFile || uploading) return;
    const file = uploadFile;
    const name = file.name;
    const key = `${file.name}:${file.size}:${file.lastModified}`;
    if (uploadKey !== key) {
      uploadKey = key;
      uploadOffset = 0;
    }
    let offset = uploadOffset;
    const total = file.size || 1;
    uploading = true;
    uploadProgress = Math.round((offset / total) * 100);
    error = '';
    const xhr = new XMLHttpRequest();
    const sendChunk = () => {
      const slice = file.slice(offset, offset + CHUNK);
      const final = offset + CHUNK >= file.size;
      xhr.open('POST', `/api/virt/images/upload?pool=${encodeURIComponent(pool)}&name=${encodeURIComponent(name)}&offset=${offset}&final=${final}`);
      xhr.setRequestHeader('Content-Type', 'application/octet-stream');
      xhr.upload.onprogress = (event) => {
        if (event.lengthComputable) {
          uploadProgress = Math.round(((offset + event.loaded) / total) * 100);
        }
      };
      xhr.onload = () => {
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
          if (/past the end/i.test(detail)) {
            uploadKey = key;
            uploadOffset = 0;
            uploadProgress = 0;
            error = 'The server no longer has the partial upload; retry to start from the beginning.';
          } else {
            error = detail;
          }
        }
      };
      xhr.onerror = () => {
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
      const name = importName.trim() || importUrl.split('/').pop()?.split('?')[0] || 'image.iso';
      const res = await fetch('/api/virt/images/import', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({
          pool,
          name,
          url: importUrl.trim(),
          sha256: importSha.trim() || null
        })
      });
      const data = await res.json();
      if (!res.ok) throw new Error(data.error || `HTTP ${res.status}`);
      importUrl = '';
      importName = '';
      importSha = '';
      // The import runs server-side; poll until it lands in the listing.
      pollImport(name);
    } catch (e: any) {
      error = e.message;
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
    if (!confirm(`Delete ${name} from the ${pool} library?`)) return;
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
    } catch (e: any) {
      error = e.message;
    }
  }
</script>

<h2 class="text-lg font-bold mb-3">Images — {pool}/vms/isos</h2>

{#if error}
  <div role="alert" class="card mb-4 border-red-700 bg-red-900/20 text-red-300 text-sm">{error}</div>
{/if}

<div class="card mb-4">
  <div class="flex items-end gap-3 flex-wrap">
    <div>
      <label class="label" for="img-file">Upload a file (ISO or cloud image)</label>
      <input id="img-file" type="file" class="input" bind:files={uploadFile} disabled={uploading} />
    </div>
    <button class="btn btn-primary" on:click={upload} disabled={!uploadFile || uploading}>
      {uploading ? `Uploading… ${uploadProgress}%` : 'Upload'}
    </button>
    {#if uploading}
      <progress class="w-48" value={uploadProgress} max="100"></progress>
    {/if}
  </div>
  <p class="text-xs text-gray-500 mt-2">
    Uploaded in 8 MiB chunks; a failed chunk resumes from the last confirmed offset. Names are flat
    (letters, digits, '-', '_', '.').
  </p>
</div>

<div class="card mb-4">
  <div class="grid grid-cols-1 md:grid-cols-4 gap-3 items-end">
    <div class="md:col-span-2">
      <label class="label" for="img-url">Import from URL</label>
      <input id="img-url" class="input w-full" bind:value={importUrl}
        placeholder="https://cloud.debian.org/…/debian-13-netinst.iso" disabled={importingUrl} />
    </div>
    <div>
      <label class="label" for="img-name">Name (optional)</label>
      <input id="img-name" class="input w-full" bind:value={importName} placeholder="from the URL" disabled={importingUrl} />
    </div>
    <button class="btn btn-primary" on:click={importFromUrl} disabled={importingUrl || !importUrl.trim()}>
      {importingUrl ? 'Starting…' : 'Import'}
    </button>
  </div>
  <div class="mt-2">
    <label class="label" for="img-sha">sha256 (optional, verified)</label>
    <input id="img-sha" class="input w-full max-w-md font-mono text-xs" bind:value={importSha}
      placeholder="64 hex chars; the download is deleted on mismatch" disabled={importingUrl} />
  </div>
  <p class="text-xs text-gray-500 mt-2">
    The node downloads the file itself (public http(s) targets only). Watch the list below for completion.
  </p>
</div>

{#if loading}
  <p class="text-sm text-gray-400">Loading library…</p>
{:else}
  {#if importing.length > 0}
    <div class="card mb-4 text-sm text-gray-300">
      Importing: {#each importing as n}<span class="text-naslos-accent">{n}</span> {/each}
    </div>
  {/if}
  {#each Object.entries(importErrors) as [name, err]}
    <div role="alert" class="card mb-2 border-red-700 bg-red-900/20 text-red-300 text-sm">
      {name}: {err}
    </div>
  {/each}
  <div class="card overflow-x-auto p-0">
    <table class="w-full text-sm">
      <thead>
        <tr class="text-left text-gray-400 border-b border-naslos-border">
          <th class="px-4 py-3">Name</th>
          <th class="px-4 py-3">Size</th>
          <th class="px-4 py-3"></th>
        </tr>
      </thead>
      <tbody>
        {#each images as img}
          <tr class="border-b border-naslos-border/50">
            <td class="px-4 py-3 font-mono text-xs">{img.name}</td>
            <td class="px-4 py-3">{formatBytes(img.sizeBytes)}</td>
            <td class="px-4 py-3 text-right">
              <button class="btn btn-secondary" on:click={() => remove(img.name)}>Delete</button>
            </td>
          </tr>
        {:else}
          <tr><td class="px-4 py-3 text-gray-500" colspan="3">The library is empty.</td></tr>
        {/each}
      </tbody>
    </table>
  </div>
{/if}
