<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import NodeCard from '$lib/components/system/NodeCard.svelte';
  import OperationsCard from '$lib/components/system/OperationsCard.svelte';
  import UpdatesCard from '$lib/components/system/UpdatesCard.svelte';
  import VersionsCard from '$lib/components/system/VersionsCard.svelte';
  import type { Op, Operation, Updates, VersionInfo } from '$lib/components/system/system';
  import { disabledReason, isActive, opTitle, safeNotesUrl } from '$lib/components/system/system';
  import { api } from '$lib/api';
  import { refreshSystemUpdates } from '$lib/stores/systemUpdates';
  import { confirmAction } from '$lib/stores/confirm';

  let version: VersionInfo | null = null;
  let updates: Updates | null = null;
  let operation: Operation | null = null;
  let loading = true;
  let loadError = '';
  let checking = false;
  let reconnecting = false;
  let message = '';
  let messageType: 'success' | 'error' | 'info' = 'success';
  let starting = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  let rebooting = false;
  let rebootTimer: ReturnType<typeof setTimeout> | undefined;
  let destroyed = false;

  async function loadVersion() {
    version = await api<VersionInfo>('/api/system/version');
  }

  async function loadUpdates() {
    updates = await api<Updates>('/api/system/updates');
  }

  async function loadOperation() {
    try {
      const next = await api<Operation>('/api/system/operations/current');
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
    } catch {
      // During the Talos reboot the API is unreachable. Keep the last state and
      // keep polling; the operation lives in a ConfigMap, not this browser.
      if (isActive(operation?.phase)) reconnecting = true;
    }
  }

  async function refresh() {
    loadError = '';
    try {
      await Promise.all([loadVersion(), loadUpdates()]);
      // Keep the sidebar update dot in sync with the page's view.
      await refreshSystemUpdates();
    } catch (e) {
      loadError = 'Failed to load system state: ' + (e instanceof Error ? e.message : String(e));
    } finally {
      loading = false;
    }
    await loadOperation();
    if (isActive(operation?.phase)) schedule();
  }

  function retry() {
    loadError = '';
    // A retry with a snapshot already on screen keeps it visible; only the
    // first load has nothing to fall back to.
    if (!version) loading = true;
    refresh();
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
      updates = await api<Updates>('/api/system/updates/check', { method: 'POST' });
      await refreshSystemUpdates();
      if (updates?.error) {
        message = 'Update check failed: ' + updates.error;
        messageType = 'error';
      } else if (updates?.available) {
        message =
          updates.available.version === version?.naslosVersion
            ? `System upgrade available for ${updates.available.version}.`
            : `Release ${updates.available.version} is available.`;
        messageType = 'success';
      } else {
        message = updates?.blocked || 'No newer release.';
        messageType = 'info';
      }
    } catch (e) {
      message = 'Update check failed: ' + (e instanceof Error ? e.message : String(e));
      messageType = 'error';
    } finally {
      checking = false;
    }
  }

  async function begin(op: Op) {
    if (!updates?.available) return;
    const reason = disabledReason(op, version, updates, operation);
    if (reason) return;
    const candidate = updates.available;
    const ok = await confirmAction({
      title: opTitle[op],
      message: `This upgrades the node to ${candidate.version}.${
        op === 'talos'
          ? ' Shares and apps go offline while the node reboots.'
          : op === 'kubernetes'
            ? ' The control plane restarts; there is no automatic rollback.'
            : ''
      }`,
      confirmLabel: 'Start upgrade',
      tone: 'danger',
      typedConfirmation: candidate.version
    });
    if (ok) await start(op, candidate.version);
  }

  async function start(op: Op, target: string) {
    starting = true;
    message = '';
    try {
      const body = await api<{ operationId?: string }>('/api/system/operations', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ op, version: target })
      });
      operation = {
        id: body.operationId ?? '',
        op,
        from: version?.naslosVersion ?? '',
        to: target,
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
      message = 'Could not start the operation: ' + (e instanceof Error ? e.message : String(e));
      messageType = 'error';
    } finally {
      starting = false;
    }
  }

  async function beginReboot() {
    if (isActive(operation?.phase)) return;
    const ok = await confirmAction({
      title: 'Reboot node',
      message: 'Shares and apps go offline while the Talos node reboots. This cannot be undone.',
      confirmLabel: 'Reboot',
      tone: 'danger',
      typedConfirmation: 'reboot'
    });
    if (ok) await rebootNode();
  }

  async function rebootNode() {
    rebooting = true;
    message = 'Rebooting the node...';
    messageType = 'info';

    let requested = false;
    let failure = '';
    const controller = new AbortController();
    const timeout = setTimeout(() => controller.abort(), 15000);
    try {
      const res = await fetch('/api/system/reboot?confirm=reboot', {
        method: 'POST',
        signal: controller.signal
      });
      const body = await res.json().catch(() => null);
      if (res.ok) {
        requested = true;
      } else if (body && typeof body.error === 'string' && body.error) {
        failure = body.error;
      } else if (res.status === 502) {
        // The proxy lost the API as the node went down; keep polling.
        requested = true;
      } else {
        failure = `HTTP ${res.status}`;
      }
    } catch {
      // Abort or dropped connection: the reboot may be underway.
      requested = true;
    } finally {
      clearTimeout(timeout);
    }

    if (destroyed) return;
    if (!requested) {
      rebooting = false;
      message = 'Could not reboot the node: ' + failure;
      messageType = 'error';
      return;
    }
    waitForReboot();
  }

  function waitForReboot() {
    const started = Date.now();
    let sawDown = false;
    const poll = async () => {
      if (destroyed) return;
      try {
        const res = await fetch('/api/system/version', { cache: 'no-store' });
        if (!res.ok) {
          sawDown = true;
        } else if (sawDown) {
          rebooting = false;
          message = 'Node is back online.';
          messageType = 'success';
          await refresh();
          return;
        }
      } catch {
        // The API is down with the node.
        sawDown = true;
      }
      if (destroyed) return;
      if (!sawDown && Date.now() - started > 2 * 60 * 1000) {
        rebooting = false;
        message = 'Reboot was requested, but the API never went down. Check the node.';
        messageType = 'error';
        return;
      }
      if (Date.now() - started > 10 * 60 * 1000) {
        rebooting = false;
        message = 'The node has not come back yet. Check the node console.';
        messageType = 'error';
        return;
      }
      rebootTimer = setTimeout(poll, 5000);
    };
    rebootTimer = setTimeout(poll, 5000);
  }

  $: notesUrl = safeNotesUrl(updates?.available?.notesUrl);
  $: operationActive = isActive(operation?.phase);

  onMount(refresh);
  onDestroy(() => {
    destroyed = true;
    if (timer) clearTimeout(timer);
    if (rebootTimer) clearTimeout(rebootTimer);
  });
</script>

<div class="mx-auto max-w-4xl">
  <PageHeader
    title="System"
    subtitle="Versions, release checks and the ordered upgrade operations."
  />

  {#if message}
    <div
      class={`callout ${messageType === 'success' ? 'callout-ok' : messageType === 'error' ? 'callout-bad' : 'callout-info'}`}
      role={messageType === 'success' ? 'status' : 'alert'}
    >
      <p class="text-sm">{message}</p>
    </div>
  {/if}

  <AsyncState
    loading={loading}
    error={version ? '' : loadError}
    onRetry={retry}
    loadingText="Loading system state..."
  >
    {#if version}
      {#if loadError}
        <div class="callout callout-bad mb-4" role="alert">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span class="text-sm">System refresh failed; showing the last snapshot. {loadError}</span>
            <Button variant="secondary" size="sm" on:click={retry}>Retry</Button>
          </div>
        </div>
      {/if}

      <VersionsCard {version} />
      <NodeCard
        {rebooting}
        operationActive={operationActive}
        onReboot={beginReboot}
      />
      <UpdatesCard
        {updates}
        {version}
        {checking}
        {notesUrl}
        onCheck={checkNow}
      />
      <OperationsCard
        {version}
        {updates}
        {operation}
        {reconnecting}
        onBegin={begin}
      />
      {#if starting}
        <p class="sr-only" role="status">Starting the operation…</p>
      {/if}
    {/if}
  </AsyncState>
</div>
