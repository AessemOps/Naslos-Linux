<script lang="ts">
  import { createEventDispatcher, onMount } from 'svelte';
  import { api, ApiError } from '$lib/api';
  import Button from '$lib/components/Button.svelte';
  import FormField from '$lib/components/FormField.svelte';
  import Modal from '$lib/components/Modal.svelte';

  interface User {
    uid: string;
    displayName: string;
    email: string;
    firstName: string;
    lastName: string;
    groups: string[];
  }

  export let user: User | null = null;

  const dispatch = createEventDispatcher<{ close: void }>();

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
      const data = await api<Array<{ cn?: string; name?: string }>>('/api/groups');
      availableGroups = (Array.isArray(data) ? data : []).map((g) => g.cn || g.name || '').filter(Boolean);
    } catch {
      // The picker stays empty; membership is preserved on save.
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
      groups = groups.filter((g) => g !== group);
    } else {
      groups = [...groups, group];
    }
  }

  function failMessage(e: unknown, fallback: string): string {
    return e instanceof ApiError ? e.message : fallback;
  }

  async function save() {
    saving = true;
    error = '';

    // A password is not part of the update endpoint: PUT /api/users/{uid}
    // handles attributes and group membership only, and the create endpoint
    // takes the password inline. Editing calls the dedicated password endpoint
    // so LDAP and the Samba NT hash both change (FR-IDN-09).
    const body: Record<string, unknown> = { uid, displayName, email, firstName, lastName, groups };

    try {
      if (!user) {
        body.password = password;
        await api('/api/users', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body)
        });
      } else {
        await api(`/api/users/${encodeURIComponent(user.uid)}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body)
        });

        if (password) {
          try {
            await api(`/api/users/${encodeURIComponent(user.uid)}/password`, {
              method: 'POST',
              headers: { 'Content-Type': 'application/json' },
              body: JSON.stringify({ password })
            });
          } catch (e) {
            throw new Error(failMessage(e, 'User saved, but the password change failed'));
          }
        }
      }
      dispatch('close');
    } catch (e) {
      error = failMessage(
        e,
        user ? 'Failed to save user' : 'Failed to create user'
      );
    } finally {
      saving = false;
    }
  }

  onMount(() => {
    loadGroups();
    init();
  });
</script>

<Modal
  open={true}
  title={user ? 'Edit User' : 'New User'}
  size="md"
  onClose={() => dispatch('close')}
>
  <form id="user-form" class="space-y-4" on:submit|preventDefault={save}>
    <FormField label="Username" id="user-field-1" required>
      <input
        id="user-field-1"
        type="text"
        bind:value={uid}
        placeholder="e.g. john"
        class="input w-full"
        disabled={!!user}
      />
    </FormField>

    <div class="grid grid-cols-1 gap-4 sm:grid-cols-2">
      <FormField label="First Name" id="user-field-2">
        <input id="user-field-2" type="text" bind:value={firstName} class="input w-full" />
      </FormField>
      <FormField label="Last Name" id="user-field-3" required>
        <input id="user-field-3" type="text" bind:value={lastName} class="input w-full" />
      </FormField>
    </div>

    <FormField label="Display Name" id="user-field-4">
      <input id="user-field-4" type="text" bind:value={displayName} class="input w-full" />
    </FormField>

    <FormField label="Email" id="user-field-5">
      <input id="user-field-5" type="email" bind:value={email} class="input w-full" />
    </FormField>

    <FormField
      label={user ? 'New Password (leave blank to keep current)' : 'Password'}
      id="user-field-6"
      required={!user}
      hint="This password is used for both web login and file shares (SMB)."
    >
      <!-- required on create only: the API rejects a passwordless new account
           (NAS-010), and editing leaves it blank to keep the current one. -->
      <input
        id="user-field-6"
        type="password"
        bind:value={password}
        required={!user}
        class="input w-full"
      />
    </FormField>

    <div class="rounded-lg border border-naslos-border bg-naslos-dark p-3 text-xs text-gray-400">
      <p class="font-medium text-gray-300">Applying a password to file shares takes a moment</p>
      <!-- Kept as single-source-line sentences: wrapping the text in the template
           puts the newline and indentation into the DOM, which splits the
           sentence when matched. -->
      <p>Web login changes immediately, but SMB picks the new password up within about 3 seconds &mdash; a share login may still be refused briefly after saving.</p>
      {#if user}
        <p>File share sessions that are already connected keep working with the old password until that client reconnects, so disconnect and reconnect (or unmount and remount) to be sure. Some clients also remember the old password and need it removed before they will ask for the new one.</p>
      {/if}
    </div>

    <div>
      <div class="label" id="user-groups-label">Groups</div>
      <div class="space-y-1" role="group" aria-labelledby="user-groups-label">
        {#each availableGroups as group (group)}
          <label class="flex min-h-10 cursor-pointer items-center gap-3 rounded px-2 hover:bg-naslos-dark">
            <input
              type="checkbox"
              checked={groups.includes(group)}
              on:change={() => toggleGroup(group)}
              class="h-4 w-4 rounded"
            />
            <span class="text-gray-300">{group}</span>
          </label>
        {/each}
      </div>
      <p class="mt-1 text-xs text-gray-500">naslos_admins = full access, naslos_users = read-only</p>
    </div>

    {#if error}
      <div role="alert" class="callout callout-bad mb-0">{error}</div>
    {/if}
  </form>

  <svelte:fragment slot="footer">
    <Button variant="secondary" on:click={() => dispatch('close')}>Cancel</Button>
    <Button
      type="submit"
      form="user-form"
      loading={saving}
      disabled={!uid || !lastName}
    >
      {saving ? 'Saving...' : user ? 'Update' : 'Create'}
    </Button>
  </svelte:fragment>
</Modal>
