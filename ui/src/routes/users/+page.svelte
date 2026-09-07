<script lang="ts">
  import { onMount } from 'svelte';
  import UserForm from '$lib/components/UserForm.svelte';

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
  let showForm = false;
  let editingUser: User | null = null;

  async function loadUsers() {
    loading = true;
    try {
      const res = await fetch('/api/users');
      users = await res.json();
    } catch (e) {
      console.error('Failed to load users:', e);
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

  async function deleteUser(uid: string) {
    if (!confirm(`Delete user "${uid}"? This will also remove their SMB access.`)) return;
    try {
      await fetch(`/api/users/${uid}`, { method: 'DELETE' });
      loadUsers();
    } catch (e) {
      console.error('Failed to delete user:', e);
    }
  }

  async function toggleUser(user: User) {
    const action = user.enabled ? 'disable' : 'enable';
    try {
      await fetch(`/api/users/${user.uid}/${action}`, { method: 'POST' });
      loadUsers();
    } catch (e) {
      console.error('Failed to toggle user:', e);
    }
  }

  onMount(loadUsers);
</script>

<div class="max-w-5xl mx-auto">
  <div class="flex justify-between items-center mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Users</h1>
      <p class="text-gray-400">Manage user accounts. One password grants access to both the web interface and file shares.</p>
    </div>
    <button class="btn btn-primary" on:click={newUser}>+ New User</button>
  </div>

  {#if loading}
    <p class="text-gray-400">Loading users...</p>
  {:else if users.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">👤</div>
      <h2 class="text-xl font-bold mb-2">No Users</h2>
      <p class="text-gray-400 mb-4">Create your first user to enable authentication.</p>
      <button class="btn btn-primary" on:click={newUser}>Create User</button>
    </div>
  {:else}
    <div class="card p-0 overflow-hidden">
      <table class="w-full">
        <thead>
          <tr class="text-left text-gray-400 text-sm border-b border-nasos-border bg-nasos-dark">
            <th class="p-4 font-medium">User</th>
            <th class="p-4 font-medium">Email</th>
            <th class="p-4 font-medium">Groups</th>
            <th class="p-4 font-medium">Status</th>
            <th class="p-4 font-medium">Actions</th>
          </tr>
        </thead>
        <tbody>
          {#each users as user}
            <tr class="border-b border-nasos-border last:border-0 hover:bg-nasos-dark/50">
              <td class="p-4">
                <div class="font-medium">{user.displayName || user.uid}</div>
                <div class="text-sm text-gray-500">{user.uid}</div>
              </td>
              <td class="p-4 text-gray-300">{user.email || '—'}</td>
              <td class="p-4">
                <div class="flex flex-wrap gap-1">
                  {#each user.groups as group}
                    <span class="text-xs px-2 py-0.5 rounded bg-nasos-border text-gray-300">{group}</span>
                  {/each}
                </div>
              </td>
              <td class="p-4">
                <span class={`text-xs px-2 py-1 rounded ${user.enabled ? 'bg-green-900/50 text-green-400' : 'bg-red-900/50 text-red-400'}`}>
                  {user.enabled ? 'Active' : 'Disabled'}
                </span>
              </td>
              <td class="p-4">
                <div class="flex gap-2">
                  <button class="btn btn-secondary text-sm" on:click={() => editUser(user)}>Edit</button>
                  <button class="btn btn-secondary text-sm" on:click={() => toggleUser(user)}>{user.enabled ? 'Disable' : 'Enable'}</button>
                  <button class="btn btn-danger text-sm" on:click={() => deleteUser(user.uid)}>Delete</button>
                </div>
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    </div>
  {/if}
</div>

{#if showForm}
  <UserForm user={editingUser} on:close={() => { showForm = false; loadUsers(); }} />
{/if}
