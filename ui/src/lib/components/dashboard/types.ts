// Shared shapes for the dashboard API payload and its child components.
export interface CpuInfo {
  usage: number;
  cores: number;
  model?: string;
  temperatureC?: number | null;
  loadAvg1?: number;
  loadAvg5?: number;
  loadAvg15?: number;
  perCore?: Array<{ core: number; usagePercent: number; frequencyMhz?: number }>;
}

export interface Pool {
  name: string;
  health: string;
  size?: number;
  alloc?: number;
  free?: number;
  usagePercent?: number;
}

export interface MemoryInfo {
  usage: number;
  total: number;
  used: number;
  available: number;
  arcSize?: number;
  arcMax?: number;
}

export interface DiskInfo {
  usage: number;
  total: number;
  used: number;
  free: number;
}

export interface SystemInfo {
  hostname: string;
  uptime: number;
  os: string;
}

export interface NetworkInfo {
  bytesSent?: number;
  bytesRecv?: number;
  interfaces?: Array<{ name: string; ipAddress?: string }>;
}

export interface DashboardData {
  cpu?: CpuInfo;
  memory?: MemoryInfo;
  disk?: DiskInfo;
  zfs?: { poolCount: number; pools: Pool[] };
  system?: SystemInfo;
  network?: NetworkInfo;
  updatedAt?: string;
}
