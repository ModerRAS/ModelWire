import type { Provider, Route, Target, ProbeResult, Metrics, ConfigExport, PaginatedLogs } from '../types';

const API_BASE = '/admin/api';

async function fetchJson<T>(url: string, options?: RequestInit): Promise<T> {
  const response = await fetch(url, {
    ...options,
    credentials: 'include',
    headers: {
      'Content-Type': 'application/json',
      ...options?.headers,
    },
  });

  if (!response.ok) {
    const error = await response.json().catch(() => ({ message: 'Request failed' }));
    throw new Error(error.message || `HTTP ${response.status}`);
  }

  return response.json();
}

// Providers API
export const providersApi = {
  list: () => fetchJson<Provider[]>(`${API_BASE}/providers`),
  get: (id: string) => fetchJson<Provider>(`${API_BASE}/providers/${id}`),
  create: (data: Partial<Provider>) => fetchJson<Provider>(`${API_BASE}/providers`, {
    method: 'POST',
    body: JSON.stringify(data),
  }),
  update: (id: string, data: Partial<Provider>) => fetchJson<Provider>(`${API_BASE}/providers/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data),
  }),
  delete: (id: string) => fetchJson<{ status: string }>(`${API_BASE}/providers/${id}`, {
    method: 'DELETE',
  }),
};

// Routes API
export const routesApi = {
  list: () => fetchJson<Route[]>(`${API_BASE}/routes`),
  get: (id: string) => fetchJson<Route>(`${API_BASE}/routes/${id}`),
  create: (data: Partial<Route>) => fetchJson<Route>(`${API_BASE}/routes`, {
    method: 'POST',
    body: JSON.stringify(data),
  }),
  update: (id: string, data: Partial<Route>) => fetchJson<Route>(`${API_BASE}/routes/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data),
  }),
  delete: (id: string) => fetchJson<{ status: string }>(`${API_BASE}/routes/${id}`, {
    method: 'DELETE',
  }),
};

// Targets API
export const targetsApi = {
  create: (routeId: string, data: Partial<Target>) => fetchJson<Target>(`${API_BASE}/routes/${routeId}/targets`, {
    method: 'POST',
    body: JSON.stringify(data),
  }),
  update: (id: string, data: Partial<Target>) => fetchJson<Target>(`${API_BASE}/targets/${id}`, {
    method: 'PATCH',
    body: JSON.stringify(data),
  }),
  delete: (id: string) => fetchJson<{ status: string }>(`${API_BASE}/targets/${id}`, {
    method: 'DELETE',
  }),
};

// Probes API
export const probesApi = {
  list: () => fetchJson<ProbeResult[]>(`${API_BASE}/probes`),
  refresh: () => fetchJson<{ status: string }>(`${API_BASE}/probes/refresh`, {
    method: 'POST',
    body: JSON.stringify({}),
  }),
};

// Logs API
export const logsApi = {
  list: (params?: { limit?: number; offset?: number; status?: string; model?: string; provider?: string }) => {
    const searchParams = new URLSearchParams();
    if (params?.limit) searchParams.set('limit', String(params.limit));
    if (params?.offset) searchParams.set('offset', String(params.offset));
    if (params?.status) searchParams.set('status', params.status);
    if (params?.model) searchParams.set('model', params.model);
    if (params?.provider) searchParams.set('provider', params.provider);
    return fetchJson<PaginatedLogs>(`${API_BASE}/logs?${searchParams}`);
  },
};

// Metrics API
export const metricsApi = {
  get: () => fetchJson<Metrics>(`${API_BASE}/metrics`),
};

// Config API
export const configApi = {
  export: () => fetchJson<ConfigExport>(`${API_BASE}/config/export`),
  import: (data: ConfigExport) => fetchJson<{ status: string }>(`${API_BASE}/config/import`, {
    method: 'POST',
    body: JSON.stringify(data),
  }),
};

// Auth API
export const authApi = {
  login: async (username: string, password: string) => {
    const response = await fetch(`${API_BASE}/auth/login`, {
      method: 'POST',
      credentials: 'include',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ username, password }),
    });
    if (!response.ok) {
      const error = await response.json().catch(() => ({ message: 'Login failed' }));
      throw new Error(error.message || `HTTP ${response.status}`);
    }
    return response.json();
  },
  logout: async () => {
    const response = await fetch(`${API_BASE}/auth/logout`, {
      method: 'POST',
      credentials: 'include',
    });
    if (!response.ok) {
      throw new Error('Logout failed');
    }
  },
  check: () => fetchJson<{ authenticated: boolean }>(`${API_BASE}/auth/check`),
};