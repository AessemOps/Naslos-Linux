<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';

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

  const dispatch = createEventDispatcher<{ close: void }>();

  let cn = '';
  let description = '';
  let selectedMembers: string[] = [];
  let saving = false;
  let error = '';

  function init() {
    if (group) {
      cn = group.cn;
      description = group.description || '';
      selectedMembers = group.members.map((m) => shortName(m));
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
      selectedMembers = selectedMembers.filter((u) => u !== uid);
    } else {
      selectedMembers = [...selectedMembers, uid];
    }
  }

  function message(e: unknown): string {
    return e instanceof Error ? e.message : 'Failed to save group';
  }

  async function save() {
    saving = true;
    error = '';

    try {
      const url = group ? `/api/groups/${encodeURIComponent(group.cn)}` : '/api/groups';
      const method = group ? 'PUT' : 'POST';
      const body = group ? { members: selectedMembers } : { cn, description };

      await api(url, {
        method,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body)
      });

      // Creating a group does not carry members in POST; apply the selection
      // with a follow-up PUT so it is not silently discarded.
      if (!group && selectedMembers.length > 0) {
        try {
          await api(`/api/groups/${encodeURIComponent(cn)}`, {
            method: 'PUT',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ members: selectedMembers })
          });
        } catch (e) {
          throw new Error(
            e instanceof ApiError
              ? `Group created, but members failed: ${e.message}`
              : 'Group created, but members failed'
          );
        }
      }
      dispatch('close');
    } catch (e) {
      error = message(e);
    } finally {
      saving = false;
    }
  }

  onMount(init);
</script>

<Modal
  open={true}
  title={group ? 'Edit Group' : 'New Group'}
  size="md"
  onClose={() => dispatch('close')}
>
  <form id="group-form" class="space-y-4" on:submit|preventDefault={save}>
    {#if !group}
      <FormField label="Group Name" id="group-field-1" required>
        <input
          id="group-field-1"
          type="text"
          bind:value={cn}
          class="input w-full"
          placeholder="e.g. naslos_users"
        />
      </FormField>
      <FormField label="Description" id="group-field-2">
        <input
          id="group-field-2"
          type="text"
          bind:value={description}
          class="input w-full"
          placeholder="e.g. Standard users"
        />
      </FormField>
    {:else}
      <p class="text-sm text-gray-400">Editing members of <strong>{group.cn}</strong>.</p>
    {/if}

    <div>
      <div class="label" id="group-members-label">Members</div>
      <div
        class="max-h-60 space-y-1 overflow-y-auto rounded-lg border border-naslos-border p-2"
        role="group"
        aria-labelledby="group-members-label"
      >
        {#each users as user (user.uid)}
          <label class="flex min-h-10 cursor-pointer items-center gap-3 rounded px-2 hover:bg-naslos-dark">
            <input
              type="checkbox"
              checked={selectedMembers.includes(user.uid)}
              on:change={() => toggleMember(user.uid)}
              class="h-4 w-4 rounded"
            />
            <span class="text-gray-300">{user.displayName || user.uid} <span class="text-sm text-gray-500">({user.uid})</span></span>
          </label>
        {/each}
        {#if users.length === 0}
          <p class="p-2 text-sm text-gray-500">No users available.</p>
        {/if}
      </div>
    </div>

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button
      type="submit"
      form="group-form"
      loading={saving}
      disabled={!group && !cn.trim()}
    >
      {saving ? 'Saving...' : group ? 'Update' : 'Create'}
    </Button>
  </svelte:fragment>
</Modal>
