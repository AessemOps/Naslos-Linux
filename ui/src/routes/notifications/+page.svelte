<script lang="ts">
  import PageHeader from '$lib/components/PageHeader.svelte';
  import NtfyChannel from '$lib/components/notifications/NtfyChannel.svelte';
  import SmtpChannel from '$lib/components/notifications/SmtpChannel.svelte';
  import SyslogChannel from '$lib/components/notifications/SyslogChannel.svelte';
  import WebhookChannel from '$lib/components/notifications/WebhookChannel.svelte';
</script>

<div class="mx-auto max-w-4xl">
  <PageHeader
    title="Notifications"
    subtitle="Configure ntfy push, the generic webhook, SMTP email and syslog. Each channel routes events and severities independently."
  />

  <div class="space-y-6">
    <NtfyChannel />
    <WebhookChannel />
    <SmtpChannel />
    <SyslogChannel />
  </div>
</div>
