<script lang="ts">
  import { onMount, createEventDispatcher } from 'svelte';

  interface User {
    uid: string;
    displayName: string;
  }

  interface Group {
    cn: string;
    description: string;
    members: string[];
  }

  export let group: Group | null = null;
  export let users: User[] = [];

  const dispatch = createEventDispatcher();

  let cn = '';
  let description = '';
  let selectedMembers: string[] = [];
  let saving = false;
  let error = '';

  function init() {
    if (group) {
      cn = group.cn;
      description = group.description || '';
      selectedMembers = group.members.map(m => shortName(m));
    }
  }

  function shortName(dnOrCn: string): string {
    if (!dnOrCn) return '';
    if (dnOrCn.startsWith('uid=') || dnOrCn.startsWith('cn=')) {
      return dnOrCn.split(',')[0].replace(/^(cn|uid)=/, '');
    }
    return dnOrCn;
  }

  function toggleMember(uid: string) {
    if (selectedMembers.includes(uid)) {
      selectedMembers = selectedMembers.filter(u => u !== uid);
    } else {
      selectedMembers = [...selectedMembers, uid];
    }
  }

  async function save() {
    saving = true;
    error = '';

    try {
      const url = group ? `/api/groups/${group.cn}` : '/api/groups';
      const method = group ? 'PUT' : 'POST';
      const body = group
        ? { members: selectedMembers }
        : { cn, description };

      const res = await fetch(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });

      if (!res.ok) {
        const data = await res.json().catch(() => ({}));
        throw new Error(data.error || `Failed to save group (HTTP ${res.status})`);
      }
      dispatch('close');
    } catch (e: any) {
      error = e.message;
    } finally {
      saving = false;
    }
  }

  onMount(init);
</script>

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{group ? 'Edit Group' : 'New Group'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      {#if !group}
        <div>
          <label class="label">Group Name *</label>
          <input type="text" bind:value={cn} class="input w-full" placeholder="e.g. naslos_users" />
        </div>
        <div>
          <label class="label">Description</label>
          <input type="text" bind:value={description} class="input w-full" placeholder="e.g. Standard users" />
        </div>
      {:else}
        <p class="text-gray-400 text-sm">Editing members of <strong>{group.cn}</strong>.</p>
      {/if}

      <div>
        <label class="label">Members</label>
        <div class="space-y-2 max-h-60 overflow-y-auto border border-naslos-border rounded-lg p-2">
          {#each users as user}
            <label class="flex items-center gap-3 p-2 rounded hover:bg-naslos-dark cursor-pointer">
              <input type="checkbox" checked={selectedMembers.includes(user.uid)} on:change={() => toggleMember(user.uid)} class="w-4 h-4 rounded" />
              <span class="text-gray-300">{user.displayName || user.uid} <span class="text-gray-500 text-sm">({user.uid})</span></span>
            </label>
          {/each}
          {#if users.length === 0}
            <p class="text-gray-500 text-sm p-2">No users available.</p>
          {/if}
        </div>
      </div>

      {#if error}<div class="p-3 rounded-lg bg-red-900/30 border border-red-700 text-red-300 text-sm">{error}</div>{/if}
    </div>

    <div class="p-6 border-t border-naslos-border flex justify-end gap-3">
      <button class="btn btn-secondary" on:click={() => dispatch('close')}>Cancel</button>
      <button class="btn btn-primary" on:click={save} disabled={saving || (!group && !cn.trim())}>
        {saving ? 'Saving...' : group ? 'Update' : 'Create'}
      </button>
    </div>
  </div>
</div>