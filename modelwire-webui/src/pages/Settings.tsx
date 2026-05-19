import { useState } from 'react';
import { useQuery, useMutation } from '@tanstack/react-query';
import { Download, Upload, AlertTriangle, CheckCircle } from 'lucide-react';
import { configApi } from '../api/client';
import type { ConfigExport } from '../types';

export function SettingsPage() {
  const [importError, setImportError] = useState('');
  const [importSuccess, setImportSuccess] = useState(false);

  const { data: config, isLoading } = useQuery({
    queryKey: ['config-export'],
    queryFn: configApi.export,
  });

  const importMutation = useMutation({
    mutationFn: (data: ConfigExport) => configApi.import(data),
    onSuccess: () => {
      setImportSuccess(true);
      setImportError('');
      setTimeout(() => setImportSuccess(false), 3000);
    },
    onError: (err: Error) => {
      setImportError(err.message || 'Import failed');
      setImportSuccess(false);
    },
  });

  const handleExport = () => {
    if (!config) return;
    const blob = new Blob([JSON.stringify(config, null, 2)], { type: 'application/json' });
    const url = URL.createObjectURL(blob);
    const a = document.createElement('a');
    a.href = url;
    a.download = `modelwire-config-${new Date().toISOString().split('T')[0]}.json`;
    document.body.appendChild(a);
    a.click();
    document.body.removeChild(a);
    URL.revokeObjectURL(url);
  };

  const handleImport = (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    if (!file) return;

    const reader = new FileReader();
    reader.onload = (event) => {
      try {
        const content = event.target?.result as string;
        const data = JSON.parse(content) as ConfigExport;
        importMutation.mutate(data);
      } catch {
        setImportError('Invalid JSON file');
      }
    };
    reader.readAsText(file);
    e.target.value = '';
  };

  if (isLoading) {
    return (
      <div className="loading-container">
        <span className="spinner" />
        Loading configuration...
      </div>
    );
  }

  return (
    <div>
      <div className="page-header">
        <h1>Settings</h1>
        <p>Configure archive settings and export/import configuration</p>
      </div>

      {importError && (
        <div className="login-error" style={{ marginBottom: '24px', display: 'flex', alignItems: 'center', gap: '8px' }}>
          <AlertTriangle size={16} />
          {importError}
        </div>
      )}
      {importSuccess && (
        <div style={{
          background: 'rgba(0, 212, 170, 0.1)',
          border: '1px solid rgba(0, 212, 170, 0.3)',
          borderRadius: '10px',
          padding: '12px 16px',
          marginBottom: '24px',
          display: 'flex',
          alignItems: 'center',
          gap: '8px',
          color: 'var(--success)',
        }}>
          <CheckCircle size={16} />
          Configuration imported successfully
        </div>
      )}

      <div className="card" style={{ marginBottom: '24px' }}>
        <div className="card-header">
          <h3 className="card-title">Archive Configuration</h3>
        </div>
        <div style={{ display: 'grid', gap: '20px' }}>
          <p style={{ color: 'var(--text-secondary)', fontSize: '14px', lineHeight: '1.6' }}>
            Configure how conversation data is archived for fine-tuning or analysis purposes.
            Archived data is stored separately from operational state and uses parseable file formats.
          </p>

          <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '24px' }}>
            <div className="form-group">
              <label className="form-label">Capture Mode</label>
              <select className="form-input form-select">
                <option value="off">Off (no archiving)</option>
                <option value="metadata_only">Metadata Only</option>
                <option value="visible_only">Visible Content Only</option>
                <option value="full_visible">Full Visible (no reasoning)</option>
                <option value="debug_raw">Debug Raw (includes reasoning)</option>
              </select>
              <p style={{ fontSize: '12px', color: 'var(--text-muted)', marginTop: '8px' }}>
                Debug mode requires explicit unsafe flag and is disabled by default
              </p>
            </div>

            <div className="form-group">
              <label className="form-label">Retention Period (days)</label>
              <input
                type="number"
                className="form-input"
                placeholder="30"
                defaultValue="30"
              />
              <p style={{ fontSize: '12px', color: 'var(--text-muted)', marginTop: '8px' }}>
                Operational state auto-expires after this period
              </p>
            </div>
          </div>

          <div style={{
            background: 'var(--bg-elevated)',
            borderRadius: '10px',
            padding: '16px',
            display: 'flex',
            alignItems: 'center',
            gap: '12px',
          }}>
            <AlertTriangle size={20} color="var(--warning)" />
            <p style={{ fontSize: '13px', color: 'var(--text-secondary)' }}>
              Raw hidden reasoning content is never archived unless explicit debug mode is enabled.
              All secrets and API keys are redacted from archived data by default.
            </p>
          </div>
        </div>
      </div>

      <div className="card">
        <div className="card-header">
          <h3 className="card-title">Configuration Management</h3>
        </div>
        <div style={{ display: 'grid', gap: '20px' }}>
          <p style={{ color: 'var(--text-secondary)', fontSize: '14px', lineHeight: '1.6' }}>
            Export your current configuration for backup or to transfer to another instance.
            Secrets are automatically redacted from exports.
          </p>

          <div style={{ display: 'flex', gap: '16px' }}>
            <button className="btn btn-primary" onClick={handleExport} disabled={!config}>
              <Download size={18} />
              Export Configuration
            </button>

            <label className="btn btn-secondary" style={{ cursor: 'pointer' }}>
              <Upload size={18} />
              Import Configuration
              <input
                type="file"
                accept=".json"
                style={{ display: 'none' }}
                onChange={handleImport}
                disabled={importMutation.isPending}
              />
            </label>
          </div>

          {config && (
            <div style={{ marginTop: '16px' }}>
              <div className="form-label">Configuration Preview</div>
              <div style={{
                background: 'var(--bg-dark)',
                border: '1px solid var(--border)',
                borderRadius: '10px',
                padding: '16px',
                maxHeight: '300px',
                overflow: 'auto',
              }}>
                <pre style={{
                  fontSize: '12px',
                  fontFamily: 'var(--font-mono)',
                  color: 'var(--text-secondary)',
                  whiteSpace: 'pre-wrap',
                }}>
                  {JSON.stringify(config, null, 2).slice(0, 2000)}
                  {JSON.stringify(config, null, 2).length > 2000 ? '...' : ''}
                </pre>
              </div>
            </div>
          )}
        </div>
      </div>

      <div className="card" style={{ marginTop: '24px' }}>
        <div className="card-header">
          <h3 className="card-title">Security Information</h3>
        </div>
        <div style={{ display: 'grid', gap: '12px', color: 'var(--text-secondary)', fontSize: '14px' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <CheckCircle size={16} color="var(--success)" />
            API keys are never displayed in logs or exports
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <CheckCircle size={16} color="var(--success)" />
            Authorization headers are redacted from all output
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <CheckCircle size={16} color="var(--success)" />
            Raw reasoning content is never stored or logged
          </div>
          <div style={{ display: 'flex', alignItems: 'center', gap: '10px' }}>
            <CheckCircle size={16} color="var(--success)" />
            Admin sessions use secure cookie-based authentication
          </div>
        </div>
      </div>
    </div>
  );
}