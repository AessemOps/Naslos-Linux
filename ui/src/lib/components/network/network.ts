// Types and pure validation for the network settings page. The API applies the
// same checks server-side; these keep an invalid edit from ever becoming a POST.
export interface Resolver {
  address: string;
  protocol: string;
}

export interface Pending {
  id: string;
  phase: string;
  link: string;
  target: string | null;
  deadline: string;
  error: string | null;
}

export interface LinkConfig {
  addressing: string;
  addresses: string[];
  routes: { destination: string | null; gateway: string }[];
  mtu: number | null;
}

export interface LinkView {
  name: string;
  kind: string;
  up: boolean;
  mac: string;
  mtu: number;
  speedMbps: number | null;
  duplex: string | null;
  addresses: { address: string; source: string }[];
  gateway: string | null;
  config: LinkConfig;
  features: { wol: string[]; ringRx: number | null; ringTx: number | null };
}

export interface Resolvers {
  inherit: boolean;
  nameservers: Resolver[];
  searchDomains: string[];
  searchInherit: boolean;
  hostDns: { enabled: boolean; forwardKubeDnsToHost: boolean };
}

export interface Inventory {
  hostname: { value: string; source: string };
  resolvers: Resolvers;
  primaryLink: string;
  links: LinkView[];
  appliance: { domain: string; lanCidr: string };
  pending: Pending | null;
}

export const emptyInventory: Inventory = {
  hostname: { value: '', source: 'auto-stable' },
  resolvers: {
    inherit: true,
    nameservers: [],
    searchDomains: [],
    searchInherit: true,
    hostDns: { enabled: false, forwardKubeDnsToHost: false }
  },
  primaryLink: '',
  links: [],
  appliance: { domain: '', lanCidr: '' },
  pending: null
};

export interface LinkFormValues {
  addressing: string;
  addresses: string;
  gateway: string;
  mtu: string;
  lowMtu: boolean;
}

export function phaseLabel(phase: string): string {
  switch (phase) {
    case 'applying':
      return 'Applying the new configuration';
    case 'verifying':
      return 'Verifying the new address';
    case 'awaiting':
      return 'Confirmed, in the revert window';
    case 'applied':
      return 'Applied';
    case 'reverted':
      return 'Reverted';
    case 'failed':
      return 'Failed';
    default:
      return phase;
  }
}

function parseV4(value: string): number[] | null {
  const parts = value.trim().split('.');
  if (parts.length !== 4) return null;
  const nums = parts.map((part) => (/^\d{1,3}$/.test(part) ? Number(part) : NaN));
  if (nums.some((n) => !Number.isInteger(n) || n < 0 || n > 255)) return null;
  return nums;
}

function ipToInt(parts: number[]): number {
  return ((parts[0] << 24) | (parts[1] << 16) | (parts[2] << 8) | parts[3]) >>> 0;
}

export function cidrError(value: string): string | null {
  const [addr, prefixText, ...rest] = value.trim().split('/');
  if (!addr || prefixText === undefined || rest.length > 0) {
    return `"${value}" is not in CIDR form (for example 192.168.1.10/24)`;
  }
  if (!/^\d{1,3}$/.test(prefixText)) return `"${value}" has an invalid prefix length`;
  const prefix = Number(prefixText);
  const v4 = parseV4(addr);
  if (v4) {
    if (prefix < 1 || prefix > 32) return `"${value}" needs an IPv4 prefix between 1 and 32`;
    if (v4[0] === 0) return `"${value}" is unspecified`;
    if (v4[0] === 127) return `"${value}" is loopback`;
    if (v4[0] === 169 && v4[1] === 254) return `"${value}" is link-local`;
    if (v4[0] >= 224) return `"${value}" is multicast or reserved`;
    return null;
  }
  if (addr.includes(':')) {
    if (!/^[0-9a-fA-F:]+$/.test(addr)) return `"${value}" is not a valid IPv6 address`;
    if (prefix < 1 || prefix > 128) return `"${value}" needs an IPv6 prefix between 1 and 128`;
    return null;
  }
  return `"${value}" is not a valid address`;
}

function inSubnet(gateway: string, cidrs: string[]): boolean {
  const g = parseV4(gateway);
  if (!g) return false;
  const gNum = ipToInt(g);
  for (const cidr of cidrs) {
    const [addr, prefixText] = cidr.split('/');
    const a = parseV4(addr || '');
    if (!a || !/^\d+$/.test(prefixText || '')) continue;
    const prefix = Number(prefixText);
    if (prefix < 1 || prefix > 32) continue;
    const mask = prefix === 32 ? 0xffffffff : (0xffffffff << (32 - prefix)) >>> 0;
    if (((ipToInt(a) & mask) >>> 0) === ((gNum & mask) >>> 0)) return true;
  }
  return false;
}

function isIp(value: string): boolean {
  if (parseV4(value)) return true;
  return value.includes(':') && /^[0-9a-fA-F:]+$/.test(value);
}

export function splitList(value: string): string[] {
  return value
    .split(/[\s,]+/)
    .map((part) => part.trim())
    .filter((part) => part !== '');
}

export function validateLink(values: LinkFormValues): string | null {
  if (values.addressing === 'static') {
    const addresses = splitList(values.addresses);
    if (addresses.length === 0 || addresses.length > 4) {
      return 'A static link needs 1-4 addresses.';
    }
    const seen = new Set<string>();
    for (const address of addresses) {
      const error = cidrError(address);
      if (error) return error;
      if (seen.has(address)) return `Duplicate address ${address}.`;
      seen.add(address);
    }
    if (values.gateway.trim() !== '') {
      if (!isIp(values.gateway.trim())) return `Gateway "${values.gateway}" is not an IP address.`;
      if (parseV4(values.gateway.trim())) {
        const v4Addresses = addresses.filter((a) => parseV4(a.split('/')[0] || ''));
        if (!inSubnet(values.gateway.trim(), v4Addresses)) {
          return `Gateway ${values.gateway} is not inside any configured subnet.`;
        }
      }
      if (addresses.some((a) => a.split('/')[0] === values.gateway.trim())) {
        return `Gateway ${values.gateway} is also a configured address.`;
      }
    }
  }
  if (values.mtu.trim() !== '') {
    const mtu = Number(values.mtu);
    if (!Number.isInteger(mtu) || mtu < 576 || mtu > 9216) {
      return 'MTU must be an integer between 576 and 9216.';
    }
    if (mtu < 1280 && !values.lowMtu) {
      return 'An MTU below 1280 needs the extra confirmation checkbox.';
    }
  }
  return null;
}

export function validateDns(
  primaryLink: string,
  nameserverMode: string,
  nameserversText: string,
  searchMode: string,
  searchText: string
): string | null {
  if (!primaryLink) {
    return 'No primary link was reported by Talos; a DNS change needs one.';
  }
  if (nameserverMode === 'explicit') {
    const servers = splitList(nameserversText);
    if (servers.length === 0 || servers.length > 3) {
      return 'Enter 1-3 DNS nameservers.';
    }
    for (const server of servers) {
      if (!isIp(server)) return `"${server}" is not an IP address.`;
    }
  }
  if (searchMode === 'explicit') {
    const domains = splitList(searchText);
    if (domains.length === 0 || domains.length > 6) {
      return 'Enter 1-6 search domains.';
    }
    for (const domain of domains) {
      if (
        !/^[a-z0-9]([a-z0-9-]*[a-z0-9])?(\.[a-z0-9]([a-z0-9-]*[a-z0-9])?)*$/.test(
          domain.toLowerCase()
        )
      ) {
        return `"${domain}" is not a valid DNS name.`;
      }
    }
  }
  if (nameserverMode === 'inherit' && searchMode === 'inherit') {
    return 'Both DNS fields already inherit from DHCP; choose an explicit value or change the link instead.';
  }
  return null;
}
