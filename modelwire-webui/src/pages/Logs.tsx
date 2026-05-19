import { useState } from 'react';
import { useQuery } from '@tanstack/react-query';
import { FileText, ChevronLeft, ChevronRight, X, Search } from 'lucide-react';
import { logsApi } from '../api/client';
import type { RequestLog } from '../types';

export function LogsPage() {
  const [filters, setFilters] = useState({
    status: '',
    model: '',
    provider: '',
  });
  const [page, setPage] = useState(0);
  const [selectedLog, setSelectedLog] = useState<RequestLog | null>(null);
  const limit = 25;

  const { data: logsData, isLoading } = useQuery({
    queryKey: ['logs', page, filters],
    queryFn: () =>
      logsApi.list({
        limit,
        offset: page * limit,
        status: filters.status || undefined,
        model: filters.model || undefined,
        provider: filters.provider || undefined,
      }),
  });

  const clearFilters = () => {
    setFilters({ status: '', model: '', provider: '' });
    setPage(0);
  };

  const hasFilters = filters.status || filters.model || filters.provider;

  if (isLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading logs...
      </div>
    );
  }

  return (
    <div>
      <div className="page-header">
        <h1>Request Logs</h1>
        <p>View and filter API request history</p>
      </div>

      <div className="filters">
        <div className="filter-group">
          <label>Status</label>
          <select
            className="filter-input"
            value={filters.status}
            onChange={(e) => {
              setFilters({ ...filters, status: e.target.value });
              setPage(0);
            }}
          >
            <option value="">All</option>
            <option value="200">2xx Success</option>
            <option value="400">4xx Client Error</option>
            <option value="500">5xx Server Error</option>
          </select>
        </div>

        <div className="filter-group">
          <label>Model</label>
          <input
            type="text"
            className="filter-input"
            placeholder="gpt-4o"
            value={filters.model}
            onChange={(e) => {
              setFilters({ ...filters, model: e.target.value });
              setPage(0);
            }}
          />
        </div>

        <div className="filter-group">
          <label>Provider</label>
          <input
            type="text"
            className="filter-input"
            placeholder="openai"
            value={filters.provider}
            onChange={(e) => {
              setFilters({ ...filters, provider: e.target.value });
              setPage(0);
            }}
          />
        </div>

        {hasFilters && (
          <button className="btn btn-secondary btn-sm" onClick={clearFilters} style={{ alignSelf: 'flex-end' }}>
            <X size={16} />
            Clear
          </button>
        )}
      </div>

      {logsData && logsData.logs.length > 0 ? (
        <>
          <div className="table-container">
            <table>
              <thead>
                <tr>
                  <th>Request ID</th>
                  <th>Model</th>
                  <th>Provider</th>
                  <th>Status</th>
                  <th>Latency</th>
                  <th>Time</th>
                  <th style={{ textAlign: 'right' }}>Details</th>
                </tr>
              </thead>
              <tbody>
                {logsData.logs.map((log) => (
                  <tr key={log.id} style={{ cursor: 'pointer' }} onClick={() => setSelectedLog(log)}>
                    <td className="mono" style={{ fontSize: '12px' }}>{log.request_id.slice(0, 16)}...</td>
                    <td className="primary">{log.downstream_model}</td>
                    <td>{log.provider}</td>
                    <td>
                      <span className={`status-badge ${
                        log.status_code < 300 ? 'success' :
                        log.status_code < 400 ? 'warning' : 'danger'
                      }`}>
                        {log.status_code}
                      </span>
                    </td>
                    <td className="mono">{log.latency_ms}ms</td>
                    <td className="mono" style={{ fontSize: '12px', color: 'var(--text-muted)' }}>
                      {new Date(log.created_at).toLocaleString()}
                    </td>
                    <td>
                      <button className="icon-btn" onClick={(e) => { e.stopPropagation(); setSelectedLog(log); }}>
                        <Search size={16} />
                      </button>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>

          <div className="pagination">
            <div className="pagination-info">
              Showing {page * limit + 1} - {Math.min((page + 1) * limit, logsData.total)} of {logsData.total}
            </div>
            <div className="pagination-controls">
              <button
                className="btn btn-secondary btn-sm"
                disabled={page === 0}
                onClick={() => setPage(page - 1)}
              >
                <ChevronLeft size={16} />
                Previous
              </button>
              <button
                className="btn btn-secondary btn-sm"
                disabled={(page + 1) * limit >= logsData.total}
                onClick={() => setPage(page + 1)}
              >
                Next
                <ChevronRight size={16} />
              </button>
            </div>
          </div>
        </>
      ) : (
        <div className="card">
          <div className="empty-state">
            <FileText size={48} />
            <h3>No logs found</h3>
            <p>{hasFilters ? 'Try adjusting your filters' : 'Request logs will appear here after API calls are made'}</p>
          </div>
        </div>
      )}

      {/* Log Detail Modal */}
      {selectedLog && (
        <div className="modal-overlay" onClick={() => setSelectedLog(null)}>
          <div className="modal" style={{ maxWidth: '600px' }} onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 className="modal-title">Request Details</h3>
              <button className="modal-close" onClick={() => setSelectedLog(null)}>
                <X size={20} />
              </button>
            </div>

            <div style={{ display: 'grid', gap: '16px' }}>
              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '16px' }}>
                <div>
                  <div className="form-label">Request ID</div>
                  <div className="mono" style={{ fontSize: '12px', color: 'var(--text-secondary)' }}>
                    {selectedLog.request_id}
                  </div>
                </div>
                <div>
                  <div className="form-label">Status Code</div>
                  <span className={`status-badge ${
                    selectedLog.status_code < 300 ? 'success' :
                    selectedLog.status_code < 400 ? 'warning' : 'danger'
                  }`}>
                    {selectedLog.status_code}
                  </span>
                </div>
              </div>

              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '16px' }}>
                <div>
                  <div className="form-label">Model</div>
                  <div>{selectedLog.downstream_model}</div>
                </div>
                <div>
                  <div className="form-label">Provider</div>
                  <div>{selectedLog.provider}</div>
                </div>
              </div>

              <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '16px' }}>
                <div>
                  <div className="form-label">Latency</div>
                  <div className="mono">{selectedLog.latency_ms}ms</div>
                </div>
                <div>
                  <div className="form-label">Timestamp</div>
                  <div style={{ fontSize: '14px', color: 'var(--text-secondary)' }}>
                    {new Date(selectedLog.created_at).toLocaleString()}
                  </div>
                </div>
              </div>

              {selectedLog.error && (
                <div>
                  <div className="form-label">Error</div>
                  <div style={{
                    background: 'rgba(255, 77, 106, 0.1)',
                    border: '1px solid rgba(255, 77, 106, 0.3)',
                    borderRadius: '8px',
                    padding: '12px',
                    color: 'var(--danger)',
                    fontSize: '13px',
                  }}>
                    {selectedLog.error}
                  </div>
                </div>
              )}
            </div>

            <div className="modal-footer">
              <button className="btn btn-secondary" onClick={() => setSelectedLog(null)}>
                Close
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}