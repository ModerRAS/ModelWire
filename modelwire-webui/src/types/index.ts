export interface Provider {
  id: string;
  name: string;
  base_url: string;
  auth_mode: string;
  default_wire_api: string;
  state_scope: string;
}

export interface Route {
  id: string;
  downstream_model: string;
  description: string;
  enabled: boolean;
  target_count: number;
}

export interface Target {
  id: string;
  route_id: string;
  provider_id: string;
  upstream_model: string;
  wire_api: string;
  priority: number;
  enabled: boolean;
  context_window_tokens?: number;
  max_output_tokens?: number;
}

export interface ProbeResult {
  key: string;
  wire_api: string;
  supports_streaming: boolean;
  supports_tools: boolean;
  last_success_at?: string;
  status: 'success' | 'failed' | 'unknown';
}

export interface RequestLog {
  id: string;
  request_id: string;
  downstream_model: string;
  provider: string;
  status_code: number;
  latency_ms: number;
  created_at: string;
  error?: string;
}

export interface Metrics {
  routes_count: number;
  providers_count: number;
  probe_cache_size: number;
}

export interface ConfigExport {
  server: Record<string, unknown>;
  security: Record<string, unknown>;
  providers: Provider[];
  routes: Route[];
}

export interface PaginatedLogs {
  logs: RequestLog[];
  total: number;
  limit: number;
  offset?: number;
}