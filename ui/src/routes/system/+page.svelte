<script lang="ts">
  import { onDestroy, onMount } from 'svelte';

  interface VersionInfo {
    naslosVersion: string;
    chartVersion: string;
    talosVersion: string;
    kubernetesVersion: string;
    releaseTalosVersion: string;
    releaseKubernetesVersion: string;
    schematicId: string;
    profile: string;
    upgradable: boolean;
    updatesEnabled: boolean;
  }

  interface Candidate {
    version: string;
    notesUrl: string;
    body: string;
    publishedAt: string;
    talosVersion: string;
    kubernetesVersion: string;
    apiImage: string;
  }

  interface Updates {
    enabled: boolean;
    upgradable: boolean;
    current: string;
    checkedAt: string;
    source?: string;
    available: Candidate | null;
    stepping: { allowed: boolean; reason: string };
    blocked: string;
    error: string;
  }

  interface Step {
    name: string;
    status: string;
    message: string;
    startedAt: string;
    finishedAt: string;
  }

  interface Operation {
    id: string;
    op: string;
    from: string;
    to: string;
    phase: string;
    steps: Step[];
    message: string;
    startedAt: string;
    finishedAt: string;
    result: string;
    attempts: number;
    log: string;
  }

  let version: VersionInfo | null = null;
  let updates: Updates | null = null;
  let operation: Operation | null = null;
  let loading = true;
  let checking = false;
  let reconnecting = false;
  let message = '';
  let messageType = '';
  const ops = ['naslos', 'talos', 'kubernetes'] as const;
  type Op = (typeof ops)[number];
  let confirmOp: Op | null = null;
  let confirmText = '';
  let starting = false;
  let timer: ReturnType<typeof setTimeout> | undefined;

  const activePhases = ['pending', 'running', 'rebooting'];

  function isActive(phase?: string): boolean {
    return !!phase && activePhases.includes(phase);
  }

  function cmpVersion(a: string, b: string): number {
    const parse = (v: string) => v.replace(/^v/, '').split('.').map((n) => parseInt(n, 10) || 0);
    const x = parse(a);
    const y = parse(b);
    for (let i = 0; i < 3; i += 1) {
      if ((x[i] ?? 0) !== (y[i] ?? 0)) return (x[i] ?? 0) - (y[i] ?? 0);
    }
    return 0;
  }

  async function loadVersion() {
    const res = await fetch('/api/system/version');
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    version = await res.json();
  }

  async function loadUpdates() {
    const res = await fetch('/api/system/updates');
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    updates = await res.json();
  }

  async function loadOperation() {
    try {
      const res = await fetch('/api/system/operations/current');
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      const next = await res.json();
      // The previous operation's ConfigMap can outlive the new Job by a few
      // seconds; never let it stop the poll for the operation we just started.
      if (
        next.phase !== 'idle' &&
        operation?.id &&
        next.id &&
        next.id !== operation.id &&
        isActive(operation.phase)
      ) {
        return;
      }
      operation = next;
      reconnecting = false;
    } catch (e) {
      // During the Talos reboot the API is unreachable. Keep the last state and
      // keep polling; the operation lives in a ConfigMap, not this browser.
      if (isActive(operation?.phase)) reconnecting = true;
    }
  }

  async function refresh() {
    try {
      await Promise.all([loadVersion(), loadUpdates()]);
    } catch (e) {
      message = 'Failed to load system state: ' + e;
      messageType = 'error';
    } finally {
      loading = false;
    }
    await loadOperation();
    if (isActive(operation?.phase)) schedule();
  }

  function schedule() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(async () => {
      await loadOperation();
      if (isActive(operation?.phase) || reconnecting) schedule();
    }, 3000);
  }

  async function checkNow() {
    checking = true;
    message = '';
    try {
      const res = await fetch('/api/system/updates/check', { method: 'POST' });
      if (!res.ok) throw new Error(`HTTP ${res.status}`);
      updates = await res.json();
      if (updates?.error) {
        message = 'Update check failed: ' + updates.error;
        messageType = 'error';
      } else if (updates?.available) {
        message = `Release ${updates.available.version} is available.`;
        messageType = 'success';
      } else {
        message = updates?.blocked || 'No newer release.';
        messageType = 'info';
      }
    } catch (e) {
      message = 'Update check failed: ' + e;
      messageType = 'error';
    } finally {
      checking = false;
    }
  }

  function disabledReason(op: Op): string {
    if (!version) return 'loading';
    if (!version.upgradable) return 'no installer release facts; bootstrap with the 0.3.0 installer or use make install-vm';
    if (isActive(operation?.phase)) return 'an operation is running';
    const candidate = updates?.available;
    if (!candidate) return 'check for updates first';
    if (!updates?.stepping.allowed) return updates?.stepping.reason || 'this release cannot be applied yet';
    if (op === 'naslos') {
      if (version.naslosVersion === candidate.version) return 'Naslos is up to date';
      if (
        version.talosVersion !== version.releaseTalosVersion ||
        version.kubernetesVersion !== version.releaseKubernetesVersion
      ) {
        return 'finish pending system upgrades first';
      }
      return '';
    }
    if (version.naslosVersion !== candidate.version) return 'upgrade Naslos first';
    if (op === 'talos') {
      if (cmpVersion(candidate.talosVersion, version.talosVersion) <= 0) return 'Talos is already current';
      return '';
    }
    if (version.talosVersion !== candidate.talosVersion) return 'upgrade Talos first';
    if (cmpVersion(candidate.kubernetesVersion, version.kubernetesVersion) <= 0) {
      return 'Kubernetes is already current';
    }
    return '';
  }

  function begin(op: Op) {
    if (disabledReason(op)) return;
    confirmText = '';
    confirmOp = op;
  }

  async function start() {
    if (!confirmOp || !updates?.available) return;
    if (confirmText !== updates.available.version) return;
    const op = confirmOp;
    const candidate = updates.available;
    starting = true;
    message = '';
    try {
      const res = await fetch('/api/system/operations', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ op, version: candidate.version })
      });
      const body = await res.json().catch(() => ({}));
      if (!res.ok) throw new Error(body.error || `HTTP ${res.status}`);
      confirmOp = null;
      operation = {
        id: body.operationId,
        op,
        from: version?.naslosVersion ?? '',
        to: candidate.version,
        phase: 'pending',
        steps: [],
        message: 'Operation created',
        startedAt: new Date().toISOString(),
        finishedAt: '',
        result: '',
        attempts: 0,
        log: ''
      };
      schedule();
    } catch (e) {
      message = 'Could not start the operation: ' + e;
      messageType = 'error';
    } finally {
      starting = false;
    }
  }

  const opTitle: Record<string, string> = {
    naslos: 'Upgrade Naslos',
    talos: 'Upgrade Talos',
    kubernetes: 'Upgrade Kubernetes'
  };

  onMount(refresh);
  onDestroy(() => {
    if (timer) clearTimeout(timer);
  });
</script>

<div class="max-w-4xl mx-auto">
  <h1 class="text-3xl font-bold mb-2">System</h1>
  <p class="text-gray-400 mb-6">Versions, release checks and the ordered upgrade operations.</p>

  {#if message}
    <div
      class="card mb-6"
      class:border-red-500={messageType === 'error'}
      class:border-green-500={messageType === 'success'}
    >
      <p class="text-sm">{message}</p>
    </div>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading system state...</p>
  {:else if version}
    <section class="card mb-6">
      <h2 class="text-lg font-bold mb-4">Versions</h2>
      <dl class="grid gap-3 sm:grid-cols-2">
        <div class="flex justify-between gap-4">
          <dt class="text-gray-400">Naslos</dt>
          <dd class="font-mono">{version.naslosVersion || '—'}</dd>
        </div>
        <div class="flex justify-between gap-4">
          <dt class="text-gray-400">Chart</dt>
          <dd class="font-mono">{version.chartVersion || '—'}</dd>
        </div>
        <div class="flex justify-between gap-4">
          <dt class="text-gray-400">Talos</dt>
          <dd class="font-mono">{version.talosVersion || '—'}</dd>
        </div>
        <div class="flex justify-between gap-4">
          <dt class="text-gray-400">Kubernetes</dt>
          <dd class="font-mono">{version.kubernetesVersion || '—'}</dd>
        </div>
      </dl>
      {#if !version.upgradable}
        <p class="text-sm text-amber-400 mt-4">
          This node has no installer release facts: it is either a
          <span class="font-mono">make install-vm</span> deployment or an install
          from a pre-0.3.0 pack. It reports versions but web upgrades are
          disabled. Use <span class="font-mono">make install-vm</span> for a VM
          deployment, or bootstrap an installer node once with the 0.3.0
          installer <span class="font-mono">--resume</span>; after that, upgrades
          run from this page.
        </p>
      {/if}
    </section>

    <section class="card mb-6">
      <div class="flex items-start justify-between gap-4">
        <div>
          <h2 class="text-lg font-bold">Updates</h2>
          <p class="text-sm text-gray-400 mt-1">
            {#if updates?.checkedAt}
              Checked {updates.checkedAt}
            {:else}
              No update check has run yet.
            {/if}
          </p>
        </div>
        <button
          type="button"
          class="btn btn-primary"
          on:click={checkNow}
          disabled={checking || !version.updatesEnabled}
        >
          {checking ? 'Checking…' : 'Check now'}
        </button>
      </div>

      {#if updates?.error}
        <p class="text-sm text-red-400 mt-3">{updates.error}</p>
      {/if}
      {#if updates?.available}
        <div class="mt-4 p-4 rounded-lg bg-naslos-dark border border-naslos-border">
          <p class="font-medium">Naslos {updates.available.version} is available</p>
          <p class="text-sm text-gray-400 mt-1">
            Talos {updates.available.talosVersion} · Kubernetes
            {updates.available.kubernetesVersion}
          </p>
          {#if updates.available.notesUrl}
            <a
              class="text-sm text-naslos-accent underline mt-2 inline-block"
              href={updates.available.notesUrl}
              target="_blank"
              rel="noreferrer"
            >Release notes</a>
          {/if}
        </div>
      {:else if updates?.blocked}
        <p class="text-sm text-gray-400 mt-3">{updates.blocked}</p>
      {/if}
    </section>

    <section class="card mb-6">
      <h2 class="text-lg font-bold mb-4">Upgrade operations</h2>
      <p class="text-sm text-gray-400 mb-4">
        The order is always Naslos, then Talos, then Kubernetes. Each step is pinned by the
        installed release.
      </p>
      <div class="space-y-3">
        {#each ops as op}
          {@const reason = disabledReason(op)}
          <div class="flex items-center justify-between gap-4 p-3 rounded-lg bg-naslos-dark">
            <div>
              <p class="font-medium">{opTitle[op]}</p>
              {#if reason}
                <p class="text-xs text-gray-500 mt-0.5">{reason}</p>
              {:else if updates?.available}
                <p class="text-xs text-gray-500 mt-0.5">
                  to {op === 'naslos'
                    ? updates.available.version
                    : op === 'talos'
                      ? updates.available.talosVersion
                      : updates.available.kubernetesVersion}
                </p>
              {/if}
            </div>
            <button
              type="button"
              class="btn btn-primary"
              disabled={!!reason}
              on:click={() => begin(op)}
            >
              {opTitle[op]}
            </button>
          </div>
        {/each}
      </div>
    </section>

    {#if operation && operation.phase !== 'idle'}
      <section class="card">
        <div class="flex items-center justify-between gap-4">
          <div>
            <h2 class="text-lg font-bold">Operation {operation.op}</h2>
            <p class="text-sm text-gray-400">
              {operation.from} → {operation.to} · <span class="font-mono">{operation.phase}</span>
            </p>
          </div>
          {#if reconnecting}
            <p class="text-sm text-amber-400">node rebooting, reconnecting…</p>
          {/if}
        </div>
        {#if operation.message}
          <p class="text-sm mt-3">{operation.message}</p>
        {/if}
        <ul class="mt-4 space-y-2">
          {#each operation.steps as step (step.name)}
            <li class="flex items-start gap-3 text-sm">
              <span
                class="mt-1 w-2 h-2 rounded-full shrink-0"
                class:bg-green-500={step.status === 'succeeded'}
                class:bg-red-500={step.status === 'failed'}
                class:bg-naslos-accent={step.status === 'running'}
                class:bg-gray-600={step.status !== 'succeeded' && step.status !== 'failed' && step.status !== 'running'}
              ></span>
              <div>
                <p class="font-medium">{step.name}</p>
                <p class="text-gray-400">{step.message}</p>
              </div>
            </li>
          {/each}
        </ul>
        {#if operation.log}
          <pre class="mt-4 p-3 rounded-lg bg-black/40 text-xs overflow-auto max-h-64 whitespace-pre-wrap">{operation.log}</pre>
        {/if}
      </section>
    {/if}
  {/if}
</div>

{#if confirmOp && updates?.available}
  <div class="fixed inset-0 bg-black/60 flex items-center justify-center p-4 z-50">
    <div class="card w-full max-w-md">
      <h2 class="text-xl font-bold mb-2">{opTitle[confirmOp]}</h2>
      <p class="text-sm text-gray-400 mb-4">
        This upgrades the node to <span class="font-mono">{updates.available.version}</span>.
        {#if confirmOp === 'talos'}
          Shares and apps go offline while the node reboots.
        {:else if confirmOp === 'kubernetes'}
          The control plane restarts; there is no automatic rollback.
        {/if}
      </p>
      <label class="label" for="confirm-version">Type {updates.available.version} to confirm</label>
      <input
        id="confirm-version"
        type="text"
        class="input w-full"
        bind:value={confirmText}
        autocomplete="off"
      />
      <div class="flex justify-end gap-3 mt-6">
        <button type="button" class="btn" on:click={() => (confirmOp = null)} disabled={starting}>
          Cancel
        </button>
        <button
          type="button"
          class="btn btn-primary"
          on:click={start}
          disabled={starting || confirmText !== updates.available.version}
        >
          {starting ? 'Starting…' : 'Start upgrade'}
        </button>
      </div>
    </div>
  </div>
{/if}
