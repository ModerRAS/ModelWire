import { useQuery } from '@tanstack/react-query';
import {
  Server,
  Route,
  Activity,
  CheckCircle,
  AlertTriangle,
} from 'lucide-react';
import { metricsApi, providersApi, routesApi } from '../api/client';
import { probesApi } from '../api/client';

export function DashboardPage() {
  const { data: metrics, isLoading: metricsLoading } = useQuery({
    queryKey: ['metrics'],
    queryFn: metricsApi.get,
    refetchInterval: 30000,
  });

  const { data: providers } = useQuery({
    queryKey: ['providers'],
    queryFn: providersApi.list,
  });

  const { data: routes } = useQuery({
    queryKey: ['routes'],
    queryFn: routesApi.list,
  });

  const { data: probes } = useQuery({
    queryKey: ['probes'],
    queryFn: probesApi.list,
  });

  const healthyProbes = probes?.filter((p) => p.status === 'success').length ?? 0;
  const totalProbes = probes?.length ?? 0;

  return (
    <div>
      <div className="page-header">
        <h1>Dashboard</h1>
        <p>System health and overview</p>
      </div>

      <div className="stats-grid">
        <div className="stat-card">
          <div className="stat-icon">
            <Route size={24} />
          </div>
          <div className="stat-value">
            {metricsLoading ? <span className="spinner" /> : metrics?.routes_count ?? 0}
          </div>
          <div className="stat-label">Active Routes</div>
        </div>

        <div className="stat-card">
          <div className="stat-icon">
            <Server size={24} />
          </div>
          <div className="stat-value">
            {metricsLoading ? <span className="spinner" /> : metrics?.providers_count ?? 0}
          </div>
          <div className="stat-label">Providers</div>
        </div>

        <div className="stat-card">
          <div className="stat-icon">
            <Activity size={24} />
          </div>
          <div className="stat-value">{totalProbes}</div>
          <div className="stat-label">Probe Results</div>
        </div>

        <div className="stat-card">
          <div className="stat-icon">
            <CheckCircle size={24} />
          </div>
          <div className="stat-value">{healthyProbes}</div>
          <div className="stat-label">Healthy Probes</div>
        </div>
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '24px' }}>
        <div className="card">
          <div className="card-header">
            <h3 className="card-title">Recent Providers</h3>
          </div>
          {providers && providers.length > 0 ? (
            <div className="table-container" style={{ border: 'none' }}>
              <table>
                <thead>
                  <tr>
                    <th>Name</th>
                    <th>Base URL</th>
                    <th>Wire API</th>
                  </tr>
                </thead>
                <tbody>
                  {providers.slice(0, 5).map((provider) => (
                    <tr key={provider.id}>
                      <td className="primary">{provider.name}</td>
                      <td className="mono" style={{ fontSize: '12px' }}>{provider.base_url}</td>
                      <td>
                        <span className="status-badge success">{provider.default_wire_api}</span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <div className="empty-state">
              <Server size={32} />
              <h3>No providers configured</h3>
              <p>Add a provider to get started</p>
            </div>
          )}
        </div>

        <div className="card">
          <div className="card-header">
            <h3 className="card-title">Recent Routes</h3>
          </div>
          {routes && routes.length > 0 ? (
            <div className="table-container" style={{ border: 'none' }}>
              <table>
                <thead>
                  <tr>
                    <th>Model</th>
                    <th>Targets</th>
                    <th>Status</th>
                  </tr>
                </thead>
                <tbody>
                  {routes.slice(0, 5).map((route) => (
                    <tr key={route.id}>
                      <td className="primary">{route.downstream_model}</td>
                      <td>{route.target_count}</td>
                      <td>
                        <span className={`status-badge ${route.enabled ? 'success' : 'warning'}`}>
                          {route.enabled ? 'Enabled' : 'Disabled'}
                        </span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          ) : (
            <div className="empty-state">
              <Route size={32} />
              <h3>No routes configured</h3>
              <p>Add a route to get started</p>
            </div>
          )}
        </div>
      </div>

      <div className="card" style={{ marginTop: '24px' }}>
        <div className="card-header">
          <h3 className="card-title">Probe Status</h3>
        </div>
        {probes && probes.length > 0 ? (
          <div className="table-container" style={{ border: 'none' }}>
            <table>
              <thead>
                <tr>
                  <th>Provider / Model</th>
                  <th>Wire API</th>
                  <th>Streaming</th>
                  <th>Tools</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {probes.slice(0, 10).map((probe) => (
                  <tr key={probe.key}>
                    <td className="primary mono" style={{ fontSize: '12px' }}>{probe.key}</td>
                    <td>{probe.wire_api}</td>
                    <td>
                      {probe.supports_streaming ? (
                        <CheckCircle size={16} color="var(--success)" />
                      ) : (
                        <AlertTriangle size={16} color="var(--warning)" />
                      )}
                    </td>
                    <td>
                      {probe.supports_tools ? (
                        <CheckCircle size={16} color="var(--success)" />
                      ) : (
                        <AlertTriangle size={16} color="var(--warning)" />
                      )}
                    </td>
                    <td>
                      <span className={`status-badge ${probe.status === 'success' ? 'success' : 'danger'}`}>
                        {probe.status}
                      </span>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="empty-state">
            <Activity size={32} />
            <h3>No probe results</h3>
            <p>Probes will appear after requests are made</p>
          </div>
        )}
      </div>
    </div>
  );
}