export interface DeviceInfo {
  device_id: string;
  device_code: string;
  access_password: string;
  device_name: string;
  local_ip: string;
  listen_port: number;
  public_ipv6?: string | null;
}

export interface AppConfig {
  device_id: string;
  device_code: string;
  access_password: string;
  device_name: string;
  listen_port: number;
  signal_server_url: string;
}

export interface TransferProgress {
  id: string;
  filename: string;
  total_bytes: number;
  transferred_bytes: number;
  speed_bps: number;
  status: 'Pending' | 'Transferring' | 'Paused' | 'Completed' | 'Failed';
}

export interface PeerDevice {
  id: string;
  name: string;
  address: string;
  status: 'online' | 'offline';
  last_seen?: string;
  mac_address?: string;
}

export interface RemoteQualitySetting {
  fps: number;
  quality: number; // 1-100
  resolution: string;
}
