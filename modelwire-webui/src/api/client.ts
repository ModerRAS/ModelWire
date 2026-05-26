import type { Provider, Route, Target, ProbeResult, Metrics, ConfigExport, PaginatedLogs } from '../types';

const API_BASE = '/admin/api';
const ADMIN_AUTH_STORAGE_KEY = 'modelwire_admin_auth_bearer';

function getStoredAdminBearer(): string | null {
  try {
    return window.localStorage.getItem(ADMIN_AUTH_STORAGE_KEY);
  } catch {
    return null;
  }
}

function setStoredAdminBearer(token: string) {
  try {
    window.localStorage.setItem(ADMIN_AUTH_STORAGE_KEY, token);
  } catch {
    // Ignore storage failures.
  }
}

function clearStoredAdminBearer() {
  try {
    window.localStorage.removeItem(ADMIN_AUTH_STORAGE_KEY);
  } catch {
    // Ignore storage failures.
  }
}

function getCookieValue(name: string): string | null {
  if (typeof document === 'undefined') {
    return null;
  }
  const cookieParts = document.cookie.split(';');
  const needle = `${name}=`;
  for (const part of cookieParts) {
    const trimmed = part.trim();
    if (trimmed.startsWith(needle)) {
      return decodeURIComponent(trimmed.slice(needle.length));
    }
  }
  return null;
}

function shouldAttachCsrfToken(method: string): boolean {
  return method === 'POST' || method === 'PUT' || method === 'PATCH' || method === 'DELETE';
}

async function fetchJson<T>(url: string, options?: RequestInit): Promise<T> {
  const method = (options?.method ?? 'GET').toUpperCase();
  const authToken = getStoredAdminBearer();
  const csrfToken = shouldAttachCsrfToken(method) ? getCookieValue('admin_csrf') : null;
  const baseHeaders: Record<string, string> = {
    'Content-Type': 'application/json',
  };
  if (authToken) {
    baseHeaders.Authorization = `Bearer ${authToken}`;
  }
  if (csrfToken) {
    baseHeaders['x-csrf-token'] = csrfToken;
  }

  const response = await fetch(url, {
    ...options,
    credentials: 'include',
    headers: {
      ...baseHeaders,
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
    const bearer = password.trim();
    if (!bearer) {
      throw new Error('Password must not be empty');
    }
    setStoredAdminBearer(bearer);
    const response = await fetch(`${API_BASE}/providers`, {
      method: 'GET',
      credentials: 'include',
      headers: { Authorization: `Bearer ${bearer}` },
    });
    if (!response.ok) {
      clearStoredAdminBearer();
      const error = await response.json().catch(() => ({ message: 'Login failed' }));
      throw new Error(error.message || `HTTP ${response.status}`);
    }
    return { authenticated: true, username };
  },
  logout: async () => {
    clearStoredAdminBearer();
  },
  check: async () => {
    const bearer = getStoredAdminBearer();
    if (!bearer) {
      return { authenticated: false };
    }
    const response = await fetch(`${API_BASE}/providers`, {
      method: 'GET',
      credentials: 'include',
      headers: { Authorization: `Bearer ${bearer}` },
    });
    if (!response.ok) {
      clearStoredAdminBearer();
      return { authenticated: false };
    }
    return { authenticated: true };
  },
};
