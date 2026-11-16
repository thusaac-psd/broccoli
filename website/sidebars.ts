import type { SidebarsConfig } from '@docusaurus/plugin-content-docs';

const sidebars: SidebarsConfig = {
  docs: [
    'intro',
    'downloads',
    {
      type: 'category',
      label: 'Using Broccoli',
      collapsed: false,
      items: ['cli/contestant', 'plugins/printing'],
    },
    {
      type: 'category',
      label: 'Running contests',
      collapsed: false,
      items: [
        'running-contests/contest-setup',
        {
          type: 'category',
          label: 'Contest formats',
          collapsed: false,
          items: [
            'running-contests/contest-formats/icpc',
            'running-contests/contest-formats/ioi',
            'plugins/codelink-qualifier',
            'plugins/codelink-bracket',
          ],
        },
        'running-contests/plugin-settings',
        'running-contests/checking-output',
      ],
    },
    {
      type: 'category',
      label: 'Building plugins',
      collapsed: false,
      items: [
        'building-plugins/getting-started',
        'building-plugins/routes',
        'building-plugins/configuration',
      ],
    },
    {
      type: 'category',
      label: 'Internals',
      collapsed: false,
      items: [
        'internals/judging',
        'internals/judging-lifecycle',
        'internals/message-queue',
        'internals/plugin-distribution',
        'internals/plugin-execution',
        'internals/plugin-database',
        'internals/schema-migrations',
        'internals/visibility',
        'internals/performance',
        'internals/sandbox-overhead',
      ],
    },
  ],
};

export default sidebars;
