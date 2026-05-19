import { useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query';
import { Plus, Edit, Trash2, X, ArrowRight } from 'lucide-react';
import { routesApi } from '../api/client';
import type { Route } from '../types';

export function RoutesPage() {
  const [isModalOpen, setIsModalOpen] = useState(false);
  const [editingRoute, setEditingRoute] = useState<Route | null>(null);
  const [deleteConfirm, setDeleteConfirm] = useState<string | null>(null);
  const queryClient = useQueryClient();
  const navigate = useNavigate();

  const { data: routes, isLoading } = useQuery({
    queryKey: ['routes'],
    queryFn: routesApi.list,
  });

  const createMutation = useMutation({
    mutationFn: (data: Partial<Route>) => routesApi.create(data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      closeModal();
    },
  });

  const updateMutation = useMutation({
    mutationFn: ({ id, data }: { id: string; data: Partial<Route> }) =>
      routesApi.update(id, data),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      closeModal();
    },
  });

  const deleteMutation = useMutation({
    mutationFn: (id: string) => routesApi.delete(id),
    onSuccess: () => {
      queryClient.invalidateQueries({ queryKey: ['routes'] });
      setDeleteConfirm(null);
    },
  });

  const openModal = (route?: Route) => {
    if (route) {
      setEditingRoute(route);
    } else {
      setEditingRoute(null);
    }
    setIsModalOpen(true);
  };

  const closeModal = () => {
    setIsModalOpen(false);
    setEditingRoute(null);
  };

  if (isLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading routes...
      </div>
    );
  }

  return (
    <div>
      <div className="page-header" style={{ display: 'flex', justifyContent: 'space-between', alignItems: 'flex-start' }}>
        <div>
          <h1>Model Routes</h1>
          <p>Configure downstream model to upstream routing</p>
        </div>
        <button className="btn btn-primary" onClick={() => openModal()}>
          <Plus size={18} />
          Add Route
        </button>
      </div>

      {routes && routes.length > 0 ? (
        <div className="table-container">
          <table>
            <thead>
              <tr>
                <th>Downstream Model</th>
                <th>Description</th>
                <th>Targets</th>
                <th>Status</th>
                <th style={{ textAlign: 'right' }}>Actions</th>
              </tr>
            </thead>
            <tbody>
              {routes.map((route) => (
                <tr key={route.id}>
                  <td className="primary mono">{route.downstream_model}</td>
                  <td>{route.description || '-'}</td>
                  <td>
                    <span className="status-badge" style={{ background: 'var(--bg-elevated)', color: 'var(--text-secondary)' }}>
                      {route.target_count} target{route.target_count !== 1 ? 's' : ''}
                    </span>
                  </td>
                  <td>
                    <span className={`status-badge ${route.enabled ? 'success' : 'warning'}`}>
                      {route.enabled ? 'Enabled' : 'Disabled'}
                    </span>
                  </td>
                  <td>
                    <div className="action-buttons">
                      <button
                        className="icon-btn"
                        onClick={() => navigate(`/admin/routes/${route.id}`)}
                        title="View Details"
                      >
                        <ArrowRight size={16} />
                      </button>
                      <button className="icon-btn" onClick={() => openModal(route)} title="Edit">
                        <Edit size={16} />
                      </button>
                      <button
                        className="icon-btn danger"
                        onClick={() => setDeleteConfirm(route.id)}
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
            <h3>No routes configured</h3>
            <p>Add a route to map downstream models to upstream targets</p>
            <button className="btn btn-primary" style={{ marginTop: '16px' }} onClick={() => openModal()}>
              Add Route
            </button>
          </div>
        </div>
      )}

      {/* Create/Edit Modal */}
      {isModalOpen && (
        <RouteModal
          route={editingRoute}
          onClose={closeModal}
          onSubmit={(data) => {
            if (editingRoute) {
              updateMutation.mutate({ id: editingRoute.id, data });
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
              <h3 className="modal-title">Delete Route</h3>
              <button className="modal-close" onClick={() => setDeleteConfirm(null)}>
                <X size={20} />
              </button>
            </div>
            <p style={{ color: 'var(--text-secondary)' }}>
              Are you sure you want to delete this route? This action cannot be undone.
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

interface RouteModalProps {
  route: Route | null;
  onClose: () => void;
  onSubmit: (data: Partial<Route>) => void;
  isLoading: boolean;
}

function RouteModal({ route, onClose, onSubmit, isLoading }: RouteModalProps) {
  const [formData, setFormData] = useState({
    downstream_model: route?.downstream_model ?? '',
    description: route?.description ?? '',
    enabled: route?.enabled ?? true,
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    onSubmit(formData);
  };

  return (
    <div className="modal-overlay" onClick={onClose}>
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <div className="modal-header">
          <h3 className="modal-title">{route ? 'Edit Route' : 'Add Route'}</h3>
          <button className="modal-close" onClick={onClose}>
            <X size={20} />
          </button>
        </div>

        <form onSubmit={handleSubmit}>
          <div className="form-group">
            <label className="form-label">Downstream Model</label>
            <input
              type="text"
              className="form-input"
              value={formData.downstream_model}
              onChange={(e) => setFormData({ ...formData, downstream_model: e.target.value })}
              placeholder="gpt-4o"
              required
            />
          </div>

          <div className="form-group">
            <label className="form-label">Description</label>
            <input
              type="text"
              className="form-input"
              value={formData.description}
              onChange={(e) => setFormData({ ...formData, description: e.target.value })}
              placeholder="Primary model for general tasks"
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
              {isLoading ? 'Saving...' : route ? 'Update' : 'Create'}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}