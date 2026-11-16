import { layout, route, type RouteConfig } from '@react-router/dev/routes';
import { flatRoutes } from '@react-router/fs-routes';

export default [
  layout('routes/_app.tsx', [
    ...(await flatRoutes({
      // Unit tests sit beside the routes they check; they are not pages.
      ignoredRouteFiles: [
        'routes/extension.tsx',
        'routes/_app.tsx',
        '**/*.test.ts',
      ],
    })),
    route('*', 'routes/extension.tsx'),
  ]),
] satisfies RouteConfig;
