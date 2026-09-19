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

    // A password is not part of the update endpoint: PUT /api/users/{uid}
    // handles attributes and group membership only, and the create endpoint
    // takes the password inline. Sending it in the PUT body (as this form used
    // to) was a silent no-op - "Update" reported success and changed nothing.
    // Editing now calls POST /api/users/{uid}/password, which updates LDAP and
    // the Samba NT hash (FR-IDN-09).
    const body: any = { uid, displayName, email, firstName, lastName, groups };

    async function failMessage(res: Response, fallback: string): Promise<string> {
      try {
        const data = await res.json();
        return data.error || fallback;
      } catch {
        return fallback;
      }
    }

    try {
      if (!user) {
        body.password = password;
        const res = await fetch('/api/users', {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body)
        });
        if (!res.ok) {
          throw new Error(await failMessage(res, 'Failed to create user'));
        }
      } else {
        const res = await fetch(`/api/users/${user.uid}`, {
          method: 'PUT',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify(body)
        });
        if (!res.ok) {
          throw new Error(await failMessage(res, 'Failed to save user'));
        }

        if (password) {
          const pwRes = await fetch(`/api/users/${user.uid}/password`, {
            method: 'POST',
            headers: { 'Content-Type': 'application/json' },
            body: JSON.stringify({ password })
          });
          if (!pwRes.ok) {
            throw new Error(await failMessage(pwRes, 'User saved, but the password change failed'));
          }
        }
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

<div class="fixed inset-0 bg-black/60 z-50 flex items-center justify-center p-4" role="presentation" on:click|self={() => dispatch('close')}>
  <div class="bg-naslos-surface rounded-2xl border border-naslos-border w-full max-w-lg max-h-[90vh] overflow-hidden flex flex-col">
    <div class="p-6 border-b border-naslos-border flex items-center justify-between">
      <h2 class="text-xl font-bold">{user ? 'Edit User' : 'New User'}</h2>
      <button class="text-gray-400 hover:text-white text-2xl" on:click={() => dispatch('close')}>×</button>
    </div>

    <div class="flex-1 overflow-y-auto p-6 space-y-4">
      <div>
        <label class="label" for="user-field-1">Username *</label>
        <input id="user-field-1" type="text" bind:value={uid} placeholder="e.g. john" class="input w-full" disabled={!!user} />
      </div>

      <div class="grid grid-cols-2 gap-4">
        <div>
          <label class="label" for="user-field-2">First Name</label>
          <input id="user-field-2" type="text" bind:value={firstName} class="input w-full" />
        </div>
        <div>
          <label class="label" for="user-field-3">Last Name *</label>
          <input id="user-field-3" type="text" bind:value={lastName} class="input w-full" />
        </div>
      </div>

      <div>
        <label class="label" for="user-field-4">Display Name</label>
        <input id="user-field-4" type="text" bind:value={displayName} class="input w-full" />
      </div>

      <div>
        <label class="label" for="user-field-5">Email</label>
        <input id="user-field-5" type="email" bind:value={email} class="input w-full" />
      </div>

      <div>
        <label class="label" for="user-field-6">{user ? 'New Password (leave blank to keep current)' : 'Password'}</label>
        <input id="user-field-6" type="password" bind:value={password} class="input w-full" />
        <p class="text-xs text-gray-500 mt-1">This password is used for both web login and file shares (SMB).</p>
        <div class="mt-2 p-3 rounded-lg bg-naslos-dark border border-naslos-border text-xs text-gray-400 space-y-1">
          <p class="font-medium text-gray-300">Applying a password to file shares takes a moment</p>
          <!-- Kept as single-source-line sentences: wrapping the text in the template puts the
               newline and indentation into the DOM, which splits the sentence when matched. -->
          <p>Web login changes immediately, but SMB picks the new password up within about 3 seconds &mdash; a share login may still be refused briefly after saving.</p>
          {#if user}
            <p>File share sessions that are already connected keep working with the old password until that client reconnects, so disconnect and reconnect (or unmount and remount) to be sure. Some clients also remember the old password and need it removed before they will ask for the new one.</p>
          {/if}
        </div>
      </div>

      <div>
        <div class="label" id="user-groups-label">Groups</div>
        <div class="space-y-2" role="group" aria-labelledby="user-groups-label">
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
