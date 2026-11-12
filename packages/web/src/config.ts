export interface AppConfig {
  api: {
    baseUrl: string;
    sessionStatusKey: string;
  };
  plugin: {
    backendUrl: string;
  };
}

export const appConfig: AppConfig = {
  api: {
    baseUrl: import.meta.env.VITE_API_BASE_URL || '/api/v1',
    sessionStatusKey: 'broccoli_session_user',
  },
  plugin: {
    backendUrl: import.meta.env.VITE_PLUGIN_BACKEND_URL || '/',
  },
};
