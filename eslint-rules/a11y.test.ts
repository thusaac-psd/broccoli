import { test } from 'node:test';

import { RuleTester } from 'eslint';

import a11y from './a11y.js';

const tester = new RuleTester({
  languageOptions: {
    parserOptions: { ecmaFeatures: { jsx: true } },
  },
});

test('icon-only controls need an accessible name', () => {
  tester.run('icon-button-label', a11y.rules['icon-button-label'], {
    valid: [
      '<Button>{t("save")}</Button>',
      '<Button><Save />{t("save")}</Button>',
      '<Button size="icon" aria-label={t("save")}><Save /></Button>',
      '<button aria-labelledby="x"><Save /></button>',
      '<Button {...props}><Save /></Button>',
      '<Button asChild><Link to="/">{t("home")}</Link></Button>',
      '<Link to="/"><img src={logo} alt={t("app.name")} /></Link>',
      '<Button><span className="sr-only">{t("close")}</span><X /></Button>',
      '<Button>{busy ? <Spinner /> : t("save")}</Button>',
    ],
    invalid: [
      '<Button size="sm"><Trash2 /></Button>',
      '<button type="button"><Plus /></button>',
      '<Button>{busy ? <Spinner /> : <Save />}</Button>',
      '<Button>{open && <ChevronUp />}</Button>',
      '<Link to="/"><img src={logo} /></Link>',
      '<a href="/"><img src={logo} alt="" /></a>',
    ].map((code) => ({ code, errors: [{ messageId: 'missing' }] })),
  });
});

test('images need alt text', () => {
  tester.run('img-alt', a11y.rules['img-alt'], {
    valid: ['<img src={a} alt={t("x")} />', '<img src={a} alt="" />'],
    invalid: [{ code: '<img src={a} />', errors: [{ messageId: 'missing' }] }],
  });
});

test('selects need a label', () => {
  tester.run('select-label', a11y.rules['select-label'], {
    valid: [
      '<select id="kind" />',
      '<select aria-label={t("kind")} />',
      '<select {...field} />',
    ],
    invalid: [
      { code: '<select value={v} />', errors: [{ messageId: 'missing' }] },
    ],
  });
});
