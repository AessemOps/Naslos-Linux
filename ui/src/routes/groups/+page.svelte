<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import EmptyState from '$lib/components/EmptyState.svelte';
  import GroupForm from '$lib/components/GroupForm.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface Group {
    cn: string;
    description: string;
    members: string[];
  }

  interface User {
    uid: string;
    displayName: string;
  }

  let groups: Group[] = [];
  let users: User[] = [];
  let loading = true;
  let loadError = '';
  let actionError = '';
  let showForm = false;
  let editingGroup: Group | null = null;
  let deleting = '';

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function loadGroups() {
    loading = true;
    loadError = '';
    try {
      const data = await api<Group[]>('/api/groups');
      groups = Array.isArray(data) ? data : [];
    } catch (e) {
      groups = [];
      loadError = `Could not load groups: ${message(e)}`;
    } finally {
      loading = false;
    }
  }

  async function loadUsers() {
    try {
      const data = await api<User[]>('/api/users');
      users = Array.isArray(data) ? data : [];
    } catch {
      // The member picker stays empty; group membership is preserved on save.
    }
  }

  function newGroup() {
    editingGroup = null;
    showForm = true;
  }

  function editGroup(group: Group) {
    editingGroup = group;
    showForm = true;
  }

  async function deleteGroup(group: Group) {
    if (
      !(await confirmAction({
        title: 'Delete group',
        message: `Delete group “${group.cn}”?`,
        confirmLabel: 'Delete',
        tone: 'danger'
      }))
    ) {
      return;
    }
    actionError = '';
    deleting = group.cn;
    try {
      await api(`/api/groups/${encodeURIComponent(group.cn)}`, { method: 'DELETE' });
      await loadGroups();
    } catch (e) {
      actionError = `Failed to delete group: ${message(e)}`;
    } finally {
      deleting = '';
    }
  }

  function shortName(dnOrCn: string): string {
    if (!dnOrCn) return '';
    if (dnOrCn.startsWith('cn=') || dnOrCn.startsWith('uid=')) {
      const part = dnOrCn.split(',')[0];
      return part.replace(/^(cn|uid)=/, '');
    }
    return dnOrCn;
  }

  onMount(() => {
    loadGroups();
    loadUsers();
  });
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Groups"
    subtitle="Manage user groups and their members."
    breadcrumbs={[{ label: 'Access' }, { label: 'Groups' }]}
  >
    <Button slot="actions" on:click={newGroup}>+ New Group</Button>
  </PageHeader>

  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}

  <AsyncState {loading} error={loadError} loadingText="Loading groups..." onRetry={loadGroups}>
    {#if groups.length === 0}
      <EmptyState
        icon="users"
        title="No Groups"
        detail="Create groups to organize users and control access."
      >
        <Button slot="action" on:click={newGroup}>Create Group</Button>
      </EmptyState>
    {:else}
      <div class="grid grid-cols-1 gap-4 md:grid-cols-2">
        {#each groups as group (group.cn)}
          <div class="card">
            <div class="mb-3 flex flex-wrap items-start justify-between gap-3">
              <div class="min-w-0">
                <h2 class="text-lg font-bold">{group.cn}</h2>
                {#if group.description}
                  <p class="text-sm text-gray-400">{group.description}</p>
                {/if}
              </div>
              <div class="flex flex-wrap gap-2">
                <Button size="sm" variant="secondary" on:click={() => editGroup(group)}>
                  Edit
                </Button>
                <Button
                  size="sm"
                  variant="danger"
                  loading={deleting === group.cn}
                  disabled={!!deleting && deleting !== group.cn}
                  on:click={() => deleteGroup(group)}
                >
                  Delete
                </Button>
              </div>
            </div>

            <div class="mb-2 text-sm text-gray-400">
              {group.members.length} member{group.members.length !== 1 ? 's' : ''}
            </div>
            {#if group.members.length > 0}
              <div class="flex flex-wrap gap-1">
                {#each group.members as member (member)}
                  <span class="badge badge-neutral">{shortName(member)}</span>
                {/each}
              </div>
            {/if}
          </div>
        {/each}
      </div>
    {/if}
  </AsyncState>
</div>

{#if showForm}
  <GroupForm
    group={editingGroup}
    {users}
    on:close={() => {
      showForm = false;
      loadGroups();
    }}
  />
{/if}
