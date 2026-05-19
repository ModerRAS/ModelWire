import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { RefreshCw, Trash2, CheckCircle, XCircle, HelpCircle } from 'lucide-react';
import { probesApi } from '../api/client';

export function ProbesPage() {
  const queryClient = useQueryClient();

  const { data: probes, isLoading } = useQuery({
    queryKey: ['probes'],
    queryFn: probesApi.list,
    refetchInterval: 60000,
  });

  const refreshMutation = useMutation({
    mutationFn: () => probesApi.refresh(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['probes'] });
    },
  });

  const clearCacheMutation = useMutation({
    mutationFn: () => probesApi.refresh(),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['probes'] });
    },
  });

  if (isLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading probe results...
      </div>
    );
  }

  return (
    <div>
      <div className="page-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start' }}>
        <div>
          <h1>Probe Status</h1>
          <p>Protocol detection results for upstream models</p>
        </div>
        <div style={{ display: 'flex', gap: '12px' }}>
          <button
            className="btn btn-secondary"
            onClick={() => clearCacheMutation.mutate()}
            disabled={clearCacheMutation.isPending}
          >
            <Trash2 size={18} />
            Clear Cache
          </button>
          <button
            className="btn btn-primary"
            onClick={() => refreshMutation.mutate()}
            disabled={refreshMutation.isPending}
          >
            <RefreshCw size={18} className={refreshMutation.isPending ? 'spinning' : ''} />
            Refresh All
          </button>
        </div>
      </div>

      {probes && probes.length > 0 ? (
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Provider / Model</th>
                <th>Wire API</th>
                <th>Streaming</th>
                <th>Tools</th>
                <th>Last Success</th>
                <th>Status</th>
                <th style={{ textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {probes.map((probe) => (
                <tr key={probe.key}>
                  <td className="primary mono" style={{ fontSize: '13px' }}>{probe.key}</td>
                  <td>
                    <span className="status-badge success">{probe.wire_api}</span>
                  </td>
                  <td>
                    {probe.supports_streaming ? (
                      <span style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--success)' }}>
                        <CheckCircle size={16} />
                        Supported
                      </span>
                    ) : (
                      <span style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--warning)' }}>
                        <XCircle size={16} />
                        Not supported
                      </span>
                    )}
                  </td>
                  <td>
                    {probe.supports_tools ? (
                      <span style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--success)' }}>
                        <CheckCircle size={16} />
                        Supported
                      </span>
                    ) : (
                      <span style={{ display: 'flex', alignItems: 'center', gap: '6px', color: 'var(--warning)' }}>
                        <XCircle size={16} />
                        Not supported
                      </span>
                    )}
                  </td>
                  <td className="mono" style={{ fontSize: '12px', color: 'var(--text-muted)' }}>
                    {probe.last_success_at ? new Date(probe.last_success_at).toLocaleString() : '-'}
                  </td>
                  <td>
                    <span className={`status-badge ${probe.status === 'success' ? 'success' : 'danger'}`}>
                      {probe.status}
                    </span>
                  </td>
                  <td>
                    <div className="action-buttons">
                      <button className="icon-btn" title="Refresh Probe">
                        <RefreshCw size={16} />
                      </button>
                    </div>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <div className="card">
          <div className="empty-state">
            <HelpCircle size={48} />
            <h3>No probe results</h3>
            <p>Probes run automatically when requests are made to unknown model/provider combinations</p>
          </div>
        </div>
      )}

      <div className="card" style={{ marginTop: '24px' }}>
        <h3 className="card-title" style={{ marginBottom: '16px' }}>About Protocol Probing</h3>
        <p style={{ color: 'var(--text-secondary)', fontSize: '14px', lineHeight: '1.6' }}>
          ModelWire uses lazy per-model protocol probing to detect which wire API format
          (OpenAI Responses, Anthropic Messages, or OpenAI Chat) each upstream provider
          supports for each model. This is necessary because gateways like New API can
          route different models to different real providers, and different providers
          may use different API formats even for the same model.
        </p>
        <p style={{ color: 'var(--text-secondary)', fontSize: '14px', lineHeight: '1.6', marginTop: '12px' }}>
          Probe results are cached and reused. The cache can be cleared or refreshed
          if you change provider configurations or need to re-detect capabilities.
        </p>
      </div>
    </div>
  );
}