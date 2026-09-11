<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';

  interface User {
    uid: string;
    displayName: string;
    email: string;
    firstName: string;
    lastName: string;
    groups: string[];
  }

  export let user: User | null = null;

  const dispatch = createEventDispatcher();

  let uid = '';
  let displayName = '';
  let email = '';
  let firstName = '';
  let lastName = '';
  let password = '';
  let groups: string[] = [];
  let availableGroups: string[] = [];
  let saving = false;
  let error = '';

  async function loadGroups() {
    try {
      const res = await fetch('/api/groups');
      if (res.ok) {
        const data = await res.json();
        availableGroups = data.map((g: any) => g.cn || g.name);
      }
    } catch (e) {
      // Ignore
    }
  }

  function init() {
    if (user) {
      uid = user.uid;
      displayName = user.displayName;
      email = user.email;
      firstName = user.firstName;
      lastName = user.lastName;
      groups = [...user.groups];
    }
  }

  function toggleGroup(group: string) {
    if (groups.includes(group)) {
      groups = groups.filter(g => g !== group);
    } else {
      groups = [...groups, group];
    }
  }

  async function save() {
    saving = true;
    error = '';

    const body: any = {
      uid,
      displayName,
      email,
      firstName,
      lastName,
      groups,
      password,
    };

    // Remove empty password for edits (don't change if not provided)
    if (user && !password) {
      delete body.password;
    }

    try {
      const url = user ? `/api/users/${user.uid}` : '/api/users';
      const method = user ? 'PUT' : 'POST';
      const res = await fetch(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });
      if (!res.ok) {
        const data = await res.json();
        throw new Error(data.error || 'Failed to save user');
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    loadGroups();
    init();
  });
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{user ? 'Edit User' : 'New User'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label">Username *</label>
        <input type="text" bind:value={uid} placeholder="e.g. john" class="input w-full" disabled={!!user} />
      </div>

      <div class="grid grid-cols-2 gap-4">
        <div>
          <label class="label">First Name</label>
          <input type="text" bind:value={firstName} class="input w-full" />
        </div>
        <div>
          <label class="label">Last Name *</label>
          <input type="text" bind:value={lastName} class="input w-full" />
        </div>
      </div>

      <div>
        <label class="label">Display Name</label>
        <input type="text" bind:value={displayName} class="input w-full" />
      </div>

      <div>
        <label class="label">Email</label>
        <input type="email" bind:value={email} class="input w-full" />
      </div>

      <div>
        <label class="label">{user ? 'New Password (leave blank to keep current)' : 'Password'}</label>
        <input type="password" bind:value={password} class="input w-full" />
        <p class="text-xs text-gray-500 mt-1">This password is used for both web login and file shares (SMB).</p>
      </div>

      <div>
        <label class="label">Groups</label>
        <div class="space-y-2">
          {#each availableGroups as group}
            <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
              <input type="checkbox" checked={groups.includes(group)} on:change={() => toggleGroup(group)} class="w-4 h-4 rounded" />
              <span class="text-gray-300">{group}</span>
            </label>
          {/each}
        </div>
        <p class="text-xs text-gray-500 mt-1">naslos_admins = full access, naslos_users = read-only</p>
      </div>

      {#if error}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>

    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || !uid || !lastName}>
        {saving ? 'Saving...' : user ? 'Update' : 'Create'}
      </button>
    </div>
  </div>
</div>
