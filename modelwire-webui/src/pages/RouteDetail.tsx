import { useState } from 'react';
import { useParams, useNavigate } from 'react-router-dom';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { ArrowLeft, Plus, Edit, Trash2, X, GripVertical } from 'lucide-react';
import { routesApi, targetsApi, providersApi } from '../api/client';
import type { Target } from '../types';

export function RouteDetailPage() {
  const { id } = useParams<{ id: string }>();
  const navigate = useNavigate();
  const queryClient = useQueryClient();
  const [editingTarget, setEditingTarget] = useState<Target | null>(null);
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);

  const { data: route, isLoading: routeLoading } = useQuery({
    queryKey: ['route', id],
    queryFn: () => routesApi.get(id!),
    enabled: !!id,
  });

  const { data: providers } = useQuery({
    queryKey: ['providers'],
    queryFn: providersApi.list,
  });

  const createTargetMutation = useMutation({
    mutationFn: (data: Partial<Target>) => targetsApi.create(id!, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['route', id] });
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      closeModal();
    },
  });

  const updateTargetMutation = useMutation({
    mutationFn: ({ id: targetId, data }: { id: string; data: Partial<Target> }) =>
      targetsApi.update(targetId, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['route', id] });
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      closeModal();
    },
  });

  const deleteTargetMutation = useMutation({
    mutationFn: (targetId: string) => targetsApi.delete(targetId),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['route', id] });
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      setDeleteConfirm(null);
    },
  });

  const openModal = (target?: Target) => {
    setEditingTarget(target ?? null);
    setIsModalOpen(true);
  };

  const closeModal = () => {
    setIsModalOpen(false);
    setEditingTarget(null);
  };

  if (routeLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading route...
      </div>
    );
  }

  if (!route) {
    return (
      <div className="card">
        <div className="empty-state">
          <h3>Route not found</h3>
          <button className="btn btn-secondary" onClick={() => navigate('/admin/routes')}>
            Back to Routes
          </button>
        </div>
      </div>
    );
  }

  // Mock targets for demo - in real implementation would come from API
  const targets: Target[] = [];

  return (
    <div>
      <div className="page-header">
        <button className="btn btn-secondary btn-sm" onClick={() => navigate('/admin/routes')} style={{ marginBottom: '16px' }}>
          <ArrowLeft size={16} />
          Back to Routes
        </button>
        <div style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start' }}>
          <div>
            <h1 style={{ fontFamily: 'var(--font-mono)', fontSize: '24px' }}>{route.downstream_model}</h1>
            <p>{route.description || 'No description'}</p>
          </div>
          <span className={`status-badge ${route.enabled ? 'success' : 'warning'}`} style={{ fontSize: '14px', padding: '8px 16px' }}>
            {route.enabled ? 'Enabled' : 'Disabled'}
          </span>
        </div>
      </div>

      <div className="card">
        <div className="card-header">
          <h3 className="card-title">Upstream Targets</h3>
          <button className="btn btn-primary btn-sm" onClick={() => openModal()}>
            <Plus size={16} />
            Add Target
          </button>
        </div>

        {targets.length > 0 ? (
          <div className="table-container" style={{ border: 'none' }}>
            <table>
              <thead>
                <tr>
                  <th style={{ width: '40px' }}></th>
                  <th>Provider</th>
                  <th>Upstream Model</th>
                  <th>Wire API</th>
                  <th>Priority</th>
                  <th>Context Window</th>
                  <th>Status</th>
                  <th style={{ textAlign: 'right' }}>Actions</th>
                </tr>
              </thead>
              <tbody>
                {targets.map((target, index) => (
                  <tr key={target.id}>
                    <td>
                      <GripVertical size={16} color="var(--text-muted)" style={{ cursor: 'grab' }} />
                    </td>
                    <td className="primary">
                      {providers?.find((p) => p.id === target.provider_id)?.name ?? target.provider_id}
                    </td>
                    <td className="mono">{target.upstream_model}</td>
                    <td>
                      <span className="status-badge success">{target.wire_api}</span>
                    </td>
                    <td>{index + 1}</td>
                    <td className="mono">
                      {target.context_window_tokens
                        ? `${(target.context_window_tokens / 1000).toFixed(0)}k tokens`
                        : '-'}
                    </td>
                    <td>
                      <span className={`status-badge ${target.enabled ? 'success' : 'warning'}`}>
                        {target.enabled ? 'Active' : 'Inactive'}
                      </span>
                    </td>
                    <td>
                      <div className="action-buttons">
                        <button className="icon-btn" onClick={() => openModal(target)} title="Edit">
                          <Edit size={16} />
                        </button>
                        <button
                          className="icon-btn danger"
                          onClick={() => setDeleteConfirm(target.id)}
                          title="Delete"
                        >
                          <Trash2 size={16} />
                        </button>
                      </div>
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <div className="empty-state">
            <Plus size={48} />
            <h3>No targets configured</h3>
            <p>Add upstream targets to route requests for this model</p>
            <button className="btn btn-primary" style={{ marginTop: '16px' }} onClick={() => openModal()}>
              Add Target
            </button>
          </div>
        )}
      </div>

      {/* Create/Edit Target Modal */}
      {isModalOpen && providers && (
        <TargetModal
          target={editingTarget}
          providers={providers}
          onClose={closeModal}
          onSubmit={(data) => {
            if (editingTarget) {
              updateTargetMutation.mutate({ id: editingTarget.id, data });
            } else {
              createTargetMutation.mutate(data);
            }
          }}
          isLoading={createTargetMutation.isPending || updateTargetMutation.isPending}
        />
      )}

      {/* Delete Confirmation */}
      {deleteConfirm && (
        <div className="modal-overlay" onClick={() => setDeleteConfirm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 className="modal-title">Delete Target</h3>
              <button className="modal-close" onClick={() => setDeleteConfirm(null)}>
                <X size={20} />
              </button>
            </div>
            <p style={{ color: 'var(--text-secondary)' }}>
              Are you sure you want to delete this target? This action cannot be undone.
            </p>
            <div className="modal-footer">
              <button className="btn btn-secondary" onClick={() => setDeleteConfirm(null)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => deleteTargetMutation.mutate(deleteConfirm)}
                disabled={deleteTargetMutation.isPending}
              >
                {deleteTargetMutation.isPending ? 'Deleting...' : 'Delete'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

interface TargetModalProps {
  target: Target | null;
  providers: { id: string; name: string }[];
  onClose: () => void;
  onSubmit: (data: Partial<Target>) => void;
  isLoading: boolean;
}

function TargetModal({ target, providers, onClose, onSubmit, isLoading }: TargetModalProps) {
  const [formData, setFormData] = useState({
    provider_id: target?.provider_id ?? (providers[0]?.id ?? ''),
    upstream_model: target?.upstream_model ?? '',
    wire_api: target?.wire_api ?? 'responses',
    priority: target?.priority ?? 1,
    enabled: target?.enabled ?? true,
    context_window_tokens: target?.context_window_tokens ?? undefined,
    max_output_tokens: target?.max_output_tokens ?? undefined,
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit(formData);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3 className="modal-title">{target ? 'Edit Target' : 'Add Target'}</h3>
          <button className="modal-close" onClick={onClose}>
            <X size={20} />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="form-group">
            <label className="form-label">Provider</label>
            <select
              className="form-input form-select"
              value={formData.provider_id}
              onChange={(e) => setFormData({ ...formData, provider_id: e.target.value })}
              required
            >
              {providers.map((p) => (
                <option key={p.id} value={p.id}>
                  {p.name}
                </option>
              ))}
            </select>
          </div>

          <div className="form-group">
            <label className="form-label">Upstream Model</label>
            <input
              type="text"
              className="form-input"
              value={formData.upstream_model}
              onChange={(e) => setFormData({ ...formData, upstream_model: e.target.value })}
              placeholder="gpt-4o"
              required
            />
          </div>

          <div className="form-group">
            <label className="form-label">Wire API</label>
            <select
              className="form-input form-select"
              value={formData.wire_api}
              onChange={(e) => setFormData({ ...formData, wire_api: e.target.value })}
            >
              <option value="responses">OpenAI Responses</option>
              <option value="anthropic">Anthropic Messages</option>
              <option value="openai_chat">OpenAI Chat</option>
            </select>
          </div>

          <div className="form-group">
            <label className="form-label">Context Window (tokens)</label>
            <input
              type="number"
              className="form-input"
              value={formData.context_window_tokens ?? ''}
              onChange={(e) =>
                setFormData({
                  ...formData,
                  context_window_tokens: e.target.value ? parseInt(e.target.value) : undefined,
                })
              }
              placeholder="128000"
            />
          </div>

          <div className="form-group">
            <label className="form-label">Max Output Tokens</label>
            <input
              type="number"
              className="form-input"
              value={formData.max_output_tokens ?? ''}
              onChange={(e) =>
                setFormData({
                  ...formData,
                  max_output_tokens: e.target.value ? parseInt(e.target.value) : undefined,
                })
              }
              placeholder="4096"
            />
          </div>

          <div className="form-group" style={{ display: 'flex', alignItems: 'center', gap: '12px' }}>
            <label className="toggle">
              <input
                type="checkbox"
                checked={formData.enabled}
                onChange={(e) => setFormData({ ...formData, enabled: e.target.checked })}
              />
              <span className="toggle-slider" />
            </label>
            <span style={{ fontSize: '14px', color: 'var(--text-secondary)' }}>Enabled</span>
          </div>

          <div className="modal-footer">
            <button type="button" className="btn btn-secondary" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={isLoading}>
              {isLoading ? 'Saving...' : target ? 'Update' : 'Create'}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}