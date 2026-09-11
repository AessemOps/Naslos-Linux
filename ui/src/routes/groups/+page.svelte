<script lang="ts">
  import { onMount } from 'svelte';
  import GroupForm from '$lib/components/GroupForm.svelte';

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
  let error = '';
  let showForm = false;
  let editingGroup: Group | null = null;

  async function loadGroups() {
    loading = true;
    error = '';
    try {
      const res = await fetch('/api/groups');
      if (res.ok) {
        const data = await res.json();
        groups = Array.isArray(data) ? data : [];
      } else {
        const data = await res.json().catch(() => ({}));
        error = data.error || `Failed to load groups (HTTP ${res.status})`;
        groups = [];
      }
    } catch (e) {
      error = 'Failed to connect to API';
      groups = [];
    } finally {
      loading = false;
    }
  }

  async function loadUsers() {
    try {
      const res = await fetch('/api/users');
      if (res.ok) {
        const data = await res.json();
        users = Array.isArray(data) ? data : [];
      }
    } catch (e) {
      // ignore
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

  async function deleteGroup(cn: string) {
    if (!confirm(`Delete group "${cn}"?`)) return;
    try {
      const res = await fetch(`/api/groups/${cn}`, { method: 'DELETE' });
      if (res.ok) {
        loadGroups();
      } else {
        const data = await res.json().catch(() => ({}));
        error = data.error || `Failed to delete group (HTTP ${res.status})`;
      }
    } catch (e) {
      error = 'Failed to connect to API';
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

<div class="max-w-5xl mx-auto">
  <div class="flex justify-between items-center mb-6">
    <div>
      <h1 class="text-3xl font-bold mb-2">Groups</h1>
      <p class="text-gray-400">Manage user groups and their members.</p>
    </div>
    <button class="btn btn-primary" on:click={newGroup}>+ New Group</button>
  </div>

  {#if error}
    <p class="text-red-400 mb-4">{error}</p>
  {/if}

  {#if loading}
    <p class="text-gray-400">Loading groups...</p>
  {:else if groups.length === 0}
    <div class="card text-center py-12">
      <div class="text-5xl mb-4">👥</div>
      <h2 class="text-xl font-bold mb-2">No Groups</h2>
      <p class="text-gray-400 mb-4">Create groups to organize users and control access.</p>
      <button class="btn btn-primary" on:click={newGroup}>Create Group</button>
    </div>
  {:else}
    <div class="grid grid-cols-1 md:grid-cols-2 gap-4">
      {#each groups as group}
        <div class="card">
          <div class="flex items-start justify-between mb-3">
            <div>
              <h3 class="font-bold text-lg">{group.cn}</h3>
              {#if group.description}
                <p class="text-sm text-gray-400">{group.description}</p>
              {/if}
            </div>
            <div class="flex gap-2">
              <button class="btn btn-secondary text-sm" on:click={() => editGroup(group)}>Edit</button>
              <button class="btn btn-danger text-sm" on:click={() => deleteGroup(group.cn)}>Delete</button>
            </div>
          </div>

          <div class="text-sm text-gray-400 mb-2">
            {group.members.length} member{group.members.length !== 1 ? 's' : ''}
          </div>
          {#if group.members.length > 0}
            <div class="flex flex-wrap gap-1">
              {#each group.members as member}
                <span class="text-xs px-2 py-0.5 rounded bg-naslos-border text-gray-300">{shortName(member)}</span>
              {/each}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
</div>

{#if showForm}
  <GroupForm group={editingGroup} {users} on:close={() => { showForm = false; loadGroups(); }} />
{/if}
