<script lang="ts">
  import { onMount } from 'svelte';
  import { api } from '$lib/api';
  import AsyncState from '$lib/components/AsyncState.svelte';
  import Button from '$lib/components/Button.svelte';
  import DataTable from '$lib/components/DataTable.svelte';
  import PageHeader from '$lib/components/PageHeader.svelte';
  import StatusBadge from '$lib/components/StatusBadge.svelte';
  import UserForm from '$lib/components/UserForm.svelte';
  import { confirmAction } from '$lib/stores/confirm';

  interface User {
    uid: string;
    displayName: string;
    email: string;
    firstName: string;
    lastName: string;
    enabled: boolean;
    groups: string[];
  }

  let users: User[] = [];
  let loading = true;
  let loadError = '';
  let actionError = '';
  let showForm = false;
  let editingUser: User | null = null;
  // The row action in flight, e.g. "jdoe:toggle"; keeps the buttons honest
  // without disabling unrelated rows.
  let busy = '';

  function message(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  async function loadUsers() {
    loading = true;
    loadError = '';
    try {
      const data = await api<User[]>('/api/users');
      users = Array.isArray(data) ? data : [];
    } catch (e) {
      users = [];
      loadError = `Could not load users: ${message(e)}`;
    } finally {
      loading = false;
    }
  }

  function newUser() {
    editingUser = null;
    showForm = true;
  }

  function editUser(user: User) {
    editingUser = user;
    showForm = true;
  }

  async function deleteUser(user: User) {
    if (
      !(await confirmAction({
        title: 'Delete user',
        message: `Delete “${user.uid}”? This will also remove their SMB access.`,
        confirmLabel: 'Delete',
        tone: 'danger'
      }))
    ) {
      return;
    }
    actionError = '';
    busy = `${user.uid}:delete`;
    try {
      await api(`/api/users/${encodeURIComponent(user.uid)}`, { method: 'DELETE' });
      await loadUsers();
    } catch (e) {
      actionError = `Failed to delete ${user.uid}: ${message(e)}`;
    } finally {
      busy = '';
    }
  }

  async function toggleUser(user: User) {
    const action = user.enabled ? 'disable' : 'enable';
    actionError = '';
    busy = `${user.uid}:toggle`;
    try {
      await api(`/api/users/${encodeURIComponent(user.uid)}/${encodeURIComponent(action)}`, {
        method: 'POST'
      });
      await loadUsers();
    } catch (e) {
      actionError = `Failed to ${action} ${user.uid}: ${message(e)}`;
    } finally {
      busy = '';
    }
  }

  onMount(loadUsers);
</script>

<div class="mx-auto max-w-6xl">
  <PageHeader
    title="Users"
    subtitle="Manage user accounts. One password grants access to both the web interface and file shares."
    breadcrumbs={[{ label: 'Access' }, { label: 'Users' }]}
  >
    <Button slot="actions" on:click={newUser}>+ New User</Button>
  </PageHeader>

  {#if actionError}
    <div role="alert" class="callout callout-bad">{actionError}</div>
  {/if}

  <AsyncState
    {loading}
    error={loadError}
    empty={users.length === 0}
    loadingText="Loading users..."
    emptyIcon="user"
    emptyTitle="No Users"
    emptyDetail="Create your first user to enable authentication."
    onRetry={loadUsers}
  >
    <svelte:fragment slot="emptyAction">
      <Button on:click={newUser}>Create User</Button>
    </svelte:fragment>
    <DataTable label="Users">
      <table class="min-w-[48rem]">
        <thead>
          <tr>
            <th scope="col">User</th>
            <th scope="col">Email</th>
            <th scope="col">Groups</th>
            <th scope="col">Status</th>
            <th scope="col">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each users as user (user.uid)}
            <tr>
              <td>
                <div class="font-medium" title={user.displayName || user.uid}>
                  {user.displayName || user.uid}
                </div>
                <div class="font-mono text-xs text-gray-500" title={user.uid}>{user.uid}</div>
              </td>
              <td class="whitespace-nowrap text-gray-300" title={user.email || ''}>
                {user.email || '—'}
              </td>
              <td>
                <div class="flex flex-wrap gap-1">
                  {#each user.groups as group (group)}
                    <span class="badge badge-neutral">{group}</span>
                  {/each}
                </div>
              </td>
              <td>
                <StatusBadge
                  state={user.enabled ? 'active' : 'disabled'}
                  label={user.enabled ? 'Active' : 'Disabled'}
                />
              </td>
              <td>
                <div class="flex flex-wrap gap-2">
                  <Button size="sm" variant="secondary" on:click={() => editUser(user)}>
                    Edit
                  </Button>
                  <Button
                    size="sm"
                    variant="secondary"
                    loading={busy === `${user.uid}:toggle`}
                    disabled={!!busy && busy !== `${user.uid}:toggle`}
                    on:click={() => toggleUser(user)}
                  >
                    {user.enabled ? 'Disable' : 'Enable'}
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    loading={busy === `${user.uid}:delete`}
                    disabled={!!busy && busy !== `${user.uid}:delete`}
                    on:click={() => deleteUser(user)}
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
</div>

{#if showForm}
  <UserForm
    user={editingUser}
    on:close={() => {
      showForm = false;
      loadUsers();
    }}
  />
{/if}
