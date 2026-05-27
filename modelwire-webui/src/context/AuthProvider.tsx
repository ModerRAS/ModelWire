import { useEffect, useState, type ReactNode } from 'react';
import { authApi } from '../api/client';
import { AuthContext } from './AuthContext';

export function AuthProvider({ children }: { children: ReactNode }) {
  const [isAuthenticated, setIsAuthenticated] = useState(false);
  const [isLoading, setIsLoading] = useState(true);

  useEffect(() => {
    let cancelled = false;
    authApi
      .check()
      .then((result) => {
        if (!cancelled) {
          setIsAuthenticated(result.authenticated);
          setIsLoading(false);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setIsAuthenticated(false);
          setIsLoading(false);
        }
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const login = async (username: string, password: string) => {
    await authApi.login(username, password);
    setIsAuthenticated(true);
  };

  const logout = async () => {
    await authApi.logout();
    setIsAuthenticated(false);
  };

  return (
    <AuthContext.Provider value={{ isAuthenticated, isLoading, login, logout }}>
      {children}
    </AuthContext.Provider>
  );
}
