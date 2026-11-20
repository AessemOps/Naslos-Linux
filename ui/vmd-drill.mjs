// Live operator drill for the VM feature (docs/virtual-machines.md, step 7).
// Drive it against the node right after freshening the session:
//   ./node_modules/.bin/playwright test --project=setup
//   node vmd-drill.mjs
// The session in tests/.auth/admin.json expires after a few idle minutes, so
// do not let the two commands drift apart.
import { request } from '@playwright/test';
import { readFileSync, existsSync } from 'node:fs';
import { generateKeyPairSync } from 'node:crypto';

if (existsSync('.env.playwright.local')) {
  for (const line of readFileSync('.env.playwright.local', 'utf8').split('\n')) {
    const m = /^\s*([A-Z0-9_]+)\s*=\s*(.*?)\s*$/.exec(line);
    if (m) process.env[m[1]] ??= m[2].replace(/^["']|["']$/g, '');
  }
}

// An ephemeral ed25519 key for cloud-init; nothing ever connects with it.
const drillSshKey = () => {
  const { publicKey } = generateKeyPairSync('ed25519');
  const raw = publicKey.export({ type: 'spki', format: 'der' }).subarray(-32);
  const name = Buffer.from('ssh-ed25519');
  const int32 = (n) => { const b = Buffer.alloc(4); b.writeUInt32BE(n); return b; };
  const buf = Buffer.concat([int32(name.length), name, int32(raw.length), raw]);
  return `ssh-ed25519 ${buf.toString('base64')} vmd-drill`;
};

const sshKey = drillSshKey();
const POOL = 'Data';
const IMAGE = 'alpine-3.22.0-x86_64-uefi.qcow2';
const VM = 'e2e-vmd-drill';

const ctx = await request.newContext({
  baseURL: process.env.PLAYWRIGHT_BASE_URL ?? 'https://naslos.local',
  ignoreHTTPSErrors: true,
  storageState: 'tests/.auth/admin.json'
});

const step = (msg) => console.log(`\n== ${msg}`);
const ok = (cond, msg) => {
  if (!cond) throw new Error(`DRILL FAILED: ${msg}`);
  console.log(`   ok: ${msg}`);
};

// Step 2: capabilities
step('capabilities');
{
  const res = await ctx.get('/api/virt/capabilities');
  const body = await res.json();
  ok(res.status() === 200, `capabilities answer ${res.status()}`);
  ok(body.kvm === true, `kvm: ${body.kvm}`);
  ok(Array.isArray(body.nics) && body.nics.length > 0, `nics: ${JSON.stringify(body.nics)}`);
}

// Step 3a: a private URL import is refused (SSRF guard). The refusal is
// asynchronous by design: the import job is accepted (202) and the guard's
// error surfaces in the images listing. TEST-NET-1 is non-public by
// definition, so the guard must refuse it.
step('private-URL import refused');
{
  const res = await ctx.post('/api/virt/images/import', {
    data: { pool: POOL, name: 'should-not-exist.iso', url: 'http://192.0.2.1/x.iso' }
  });
  ok(res.status() === 202, `import job accepted: ${res.status()}`);
  let refused = '';
  for (let i = 0; i < 10; i++) {
    await new Promise((r) => setTimeout(r, 2000));
    const images = await (await ctx.get(`/api/virt/images?pool=${POOL}`)).json();
    refused = (images.errors || {})[`should-not-exist.iso`] || '';
    if (refused) break;
  }
  ok(refused.includes('non-public'), `SSRF refusal surfaced: ${refused}`);
}

// Step 3b: import the cloud image by URL
step('import cloud image by URL');
{
  const res = await ctx.post('/api/virt/images/import', {
    data: {
      pool: POOL,
      name: IMAGE,
      url: 'https://dl-cdn.alpinelinux.org/alpine/v3.22/releases/cloud/nocloud_alpine-3.22.0-x86_64-uefi-cloudinit-r0.qcow2'
    }
  });
  ok(res.status() === 202, `import accepted: ${res.status()} ${await res.text()}`);
  // Poll until the import completes (194 MB, server-side).
  let done = false;
  for (let i = 0; i < 60; i++) {
    await new Promise((r) => setTimeout(r, 8000));
    const listing = await (await ctx.get(`/api/virt/images?pool=${POOL}`)).json();
    const img = (listing.images || []).find((x) => x.name === IMAGE);
    if (img && img.sizeBytes > 0) {
      console.log(`   imported: ${img.name} (${Math.round(img.sizeBytes / 1e6)} MB)`);
      done = true;
      break;
    }
    const err = (listing.errors || {})[IMAGE];
    if (err) throw new Error(`DRILL FAILED: import error: ${err}`);
  }
  ok(done, 'import completed');
}

// Step 4: create the VM (cloud-init on, autostart off)
step('create VM');
{
  const res = await ctx.post('/api/virt/vms', {
    data: {
      name: VM,
      pool: POOL,
      cpu: 1,
      memMiB: 256,
      autostart: false,
      disk: { sizeGB: 2, format: 'qcow2', bus: 'virtio', sourceImage: `isos/${IMAGE}` },
      net: { mode: 'nat', forwards: [] },
      cloudInit: { hostname: VM, user: 'alpine', sshKeys: [sshKey] }
    }
  });
  ok(res.status() < 300, `create accepted: ${res.status()} ${await res.text()}`);
}

// Step 5: start -> running
step('start VM');
{
  const res = await ctx.post(`/api/virt/vms/${VM}/start`);
  ok(res.status() < 300, `start accepted: ${res.status()}`);
  let running = false;
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 3000));
    const vms = await (await ctx.get('/api/virt/vms')).json();
    const vm = Array.isArray(vms) ? vms.find((v) => v.name === VM) : vms;
    if (vm && vm.state === 'running') {
      console.log(`   state: running, unexpectedExit=${vm.unexpectedExit}`);
      // The listing never carries console credentials (they only exist on
      // the /api/ws/vm/{name}/vnc preflight; see vms.spec.ts).
      ok(vm.vncPort === undefined, 'listing does not leak the console port');
      const pre = await ctx.get(`/api/ws/vm/${VM}/vnc`);
      const body = await pre.json();
      ok(pre.status() === 200, `console preflight: ${pre.status()} ${JSON.stringify(body)}`);
      ok(body.vncPort > 0, `vncPort assigned: ${body.vncPort}`);
      ok(typeof body.vncPassword === 'string' && body.vncPassword.length > 0, 'per-boot VNC password present');
      running = true;
      break;
    }
    if (vm && vm.unexpectedExit) throw new Error(`DRILL FAILED: QEMU exited: ${vm.lastError}`);
  }
  ok(running, 'VM reached running state');
}

// Step 6a: graceful stop
step('graceful stop (ACPI)');
{
  const res = await ctx.post(`/api/virt/vms/${VM}/stop`);
  ok(res.status() < 300, `stop accepted: ${res.status()}`);
  let stopped = false;
  for (let i = 0; i < 30; i++) {
    await new Promise((r) => setTimeout(r, 3000));
    const vms = await (await ctx.get('/api/virt/vms')).json();
    const vm = Array.isArray(vms) ? vms.find((v) => v.name === VM) : vms;
    if (vm && vm.state === 'stopped') {
      stopped = true;
      break;
    }
  }
  ok(stopped, 'VM reached stopped state');
}

// Step 6b: snapshot, then restore while stopped
step('snapshot + restore while stopped');
{
  const res = await ctx.post(`/api/virt/vms/${VM}/snapshots`, {
    data: { name: 'drill-snap' }
  });
  ok(res.status() < 300, `snapshot accepted: ${res.status()} ${await res.text()}`);
  const list = await (await ctx.get(`/api/virt/vms/${VM}/snapshots`)).json();
  ok(JSON.stringify(list).includes('drill-snap'), 'snapshot listed');
  const restore = await ctx.post(`/api/virt/vms/${VM}/snapshots/drill-snap/restore`);
  ok(restore.status() < 300, `restore (stopped) accepted: ${restore.status()}`);
}

// Step 6c: restore refuses while running
step('restore refuses while running');
{
  const start = await ctx.post(`/api/virt/vms/${VM}/start`);
  ok(start.status() < 300, 'restarted');
  for (let i = 0; i < 20; i++) {
    await new Promise((r) => setTimeout(r, 3000));
    const vms = await (await ctx.get('/api/virt/vms')).json();
    const vm = Array.isArray(vms) ? vms.find((v) => v.name === VM) : vms;
    if (vm && vm.state === 'running') break;
  }
  const restore = await ctx.post(`/api/virt/vms/${VM}/snapshots/drill-snap/restore`);
  ok(restore.status() === 409, `restore while running refused: ${restore.status()} ${(await restore.json()).error || ''}`);
}

// Step 6d: force stop, then delete
step('force stop + delete');
{
  const force = await ctx.post(`/api/virt/vms/${VM}/force-stop`);
  ok(force.status() < 300, `force-stop accepted: ${force.status()}`);
  for (let i = 0; i < 20; i++) {
    await new Promise((r) => setTimeout(r, 3000));
    const vms = await (await ctx.get('/api/virt/vms')).json();
    const vm = Array.isArray(vms) ? vms.find((v) => v.name === VM) : vms;
    if (vm && vm.state === 'stopped') break;
  }
  const del = await ctx.delete(`/api/virt/vms/${VM}`);
  ok(del.status() < 300, `delete accepted: ${del.status()} ${await del.text()}`);
  const ds = await (await ctx.get('/api/datasets')).json();
  ok(
    !JSON.stringify(ds).includes(`${POOL}/vms/${VM}`),
    `dataset ${POOL}/vms/${VM} destroyed`
  );
}

// Cleanup: remove the imported image too.
step('delete imported image');
{
  const res = await ctx.delete(`/api/virt/images/${IMAGE}?pool=${POOL}`);
  ok(res.status() < 300, `image deleted: ${res.status()} ${await res.text()}`);
}

await ctx.dispose();
console.log('\nLIVE DRILL PASSED');
