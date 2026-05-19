import { useState } from 'react';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Plus, Edit, Trash2, TestTube, X } from 'lucide-react';
import { providersApi } from '../api/client';
import type { Provider } from '../types';

export function ProvidersPage() {
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [editingProvider, setEditingProvider] = useState<Provider | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  const queryClient = useQueryClient();

  const { data: providers, isLoading } = useQuery({
    queryKey: ['providers'],
    queryFn: providersApi.list,
  });

  const createMutation = useMutation({
    mutationFn: (data: Partial<Provider>) => providersApi.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      closeModal();
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, data }: { id: string; data: Partial<Provider> }) =>
      providersApi.update(id, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      closeModal();
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => providersApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['providers'] });
      setDeleteConfirm(null);
    },
  });

  const openModal = (provider?: Provider) => {
    if (provider) {
      setEditingProvider(provider);
    } else {
      setEditingProvider(null);
    }
    setIsModalOpen(true);
  };

  const closeModal = () => {
    setIsModalOpen(false);
    setEditingProvider(null);
  };

  if (isLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading providers...
      </div>
    );
  }

  return (
    <div>
      <div className="page-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start' }}>
        <div>
          <h1>Providers</h1>
          <p>Manage upstream model providers</p>
        </div>
        <button className="btn btn-primary" onClick={() => openModal()}>
          <Plus size={18} />
          Add Provider
        </button>
      </div>

      {providers && providers.length > 0 ? (
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Name</th>
                <th>Base URL</th>
                <th>Auth Mode</th>
                <th>Wire API</th>
                <th>State Scope</th>
                <th style={{ textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {providers.map((provider) => (
                <tr key={provider.id}>
                  <td className="primary">{provider.name}</td>
                  <td className="mono" style={{ fontSize: '12px' }}>{provider.base_url}</td>
                  <td>{provider.auth_mode}</td>
                  <td>
                    <span className="status-badge success">{provider.default_wire_api}</span>
                  </td>
                  <td className="mono">{provider.state_scope}</td>
                  <td>
                    <div className="action-buttons">
                      <button className="icon-btn" title="Test Connection">
                        <TestTube size={16} />
                      </button>
                      <button className="icon-btn" onClick={() => openModal(provider)} title="Edit">
                        <Edit size={16} />
                      </button>
                      <button
                        className="icon-btn danger"
                        onClick={() => setDeleteConfirm(provider.id)}
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
        <div className="card">
          <div className="empty-state">
            <Plus size={48} />
            <h3>No providers yet</h3>
            <p>Add your first provider to start routing models</p>
            <button className="btn btn-primary" style={{ marginTop: '16px' }} onClick={() => openModal()}>
              Add Provider
            </button>
          </div>
        </div>
      )}

      {/* Create/Edit Modal */}
      {isModalOpen && (
        <ProviderModal
          provider={editingProvider}
          onClose={closeModal}
          onSubmit={(data) => {
            if (editingProvider) {
              updateMutation.mutate({ id: editingProvider.id, data });
            } else {
              createMutation.mutate(data);
            }
          }}
          isLoading={createMutation.isPending || updateMutation.isPending}
        />
      )}

      {/* Delete Confirmation */}
      {deleteConfirm && (
        <div className="modal-overlay" onClick={() => setDeleteConfirm(null)}>
          <div className="modal" onClick={(e) => e.stopPropagation()}>
            <div className="modal-header">
              <h3 className="modal-title">Delete Provider</h3>
              <button className="modal-close" onClick={() => setDeleteConfirm(null)}>
                <X size={20} />
              </button>
            </div>
            <p style={{ color: 'var(--text-secondary)' }}>
              Are you sure you want to delete this provider? This action cannot be undone.
            </p>
            <div className="modal-footer">
              <button className="btn btn-secondary" onClick={() => setDeleteConfirm(null)}>
                Cancel
              </button>
              <button
                className="btn btn-danger"
                onClick={() => deleteMutation.mutate(deleteConfirm)}
                disabled={deleteMutation.isPending}
              >
                {deleteMutation.isPending ? 'Deleting...' : 'Delete'}
              </button>
            </div>
          </div>
        </div>
      )}
    </div>
  );
}

interface ProviderModalProps {
  provider: Provider | null;
  onClose: () => void;
  onSubmit: (data: Partial<Provider>) => void;
  isLoading: boolean;
}

function ProviderModal({ provider, onClose, onSubmit, isLoading }: ProviderModalProps) {
  const [formData, setFormData] = useState({
    name: provider?.name ?? '',
    base_url: provider?.base_url ?? '',
    auth_mode: provider?.auth_mode ?? 'bearer',
    default_wire_api: provider?.default_wire_api ?? 'responses',
    state_scope: provider?.state_scope ?? 'default',
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit(formData);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3 className="modal-title">{provider ? 'Edit Provider' : 'Add Provider'}</h3>
          <button className="modal-close" onClick={onClose}>
            <X size={20} />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="form-group">
            <label className="form-label">Name</label>
            <input
              type="text"
              className="form-input"
              value={formData.name}
              onChange={(e) => setFormData({ ...formData, name: e.target.value })}
              placeholder="OpenAI"
              required
            />
          </div>

          <div className="form-group">
            <label className="form-label">Base URL</label>
            <input
              type="url"
              className="form-input"
              value={formData.base_url}
              onChange={(e) => setFormData({ ...formData, base_url: e.target.value })}
              placeholder="https://api.openai.com/v1"
              required
            />
          </div>

          <div className="form-group">
            <label className="form-label">Auth Mode</label>
            <select
              className="form-input form-select"
              value={formData.auth_mode}
              onChange={(e) => setFormData({ ...formData, auth_mode: e.target.value })}
            >
              <option value="bearer">Bearer Token</option>
              <option value="api_key">API Key</option>
              <option value="none">None</option>
            </select>
          </div>

          <div className="form-group">
            <label className="form-label">Default Wire API</label>
            <select
              className="form-input form-select"
              value={formData.default_wire_api}
              onChange={(e) => setFormData({ ...formData, default_wire_api: e.target.value })}
            >
              <option value="responses">OpenAI Responses</option>
              <option value="anthropic">Anthropic Messages</option>
              <option value="openai_chat">OpenAI Chat</option>
            </select>
          </div>

          <div className="form-group">
            <label className="form-label">State Scope</label>
            <input
              type="text"
              className="form-input"
              value={formData.state_scope}
              onChange={(e) => setFormData({ ...formData, state_scope: e.target.value })}
              placeholder="default"
            />
          </div>

          <div className="modal-footer">
            <button type="button" className="btn btn-secondary" onClick={onClose}>
              Cancel
            </button>
            <button type="submit" className="btn btn-primary" disabled={isLoading}>
              {isLoading ? 'Saving...' : provider ? 'Update' : 'Create'}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}