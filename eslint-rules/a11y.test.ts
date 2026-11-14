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

test('inputs need a name, not just a placeholder', () => {
  tester.run('input-label', a11y.rules['input-label'], {
    valid: [
      '<Input aria-label={t("search")} placeholder={t("search")} />',
      '<input type="checkbox" aria-label={t("select")} />',
      '<Input id="name" />',
      '<Label>{t("name")}<Input /></Label>',
      '<label><input type="checkbox" />{t("all")}</label>',
      '<input type="hidden" name="x" />',
      '<input type="file" className="hidden" onChange={pick} />',
      '<Switch {...props} />',
    ],
    invalid: [
      '<Input placeholder={t("search")} />',
      '<input type="checkbox" checked={on} />',
      '<Switch checked={on} />',
      '<Textarea placeholder={t("note")} />',
      '<SelectTrigger><SelectValue placeholder={t("status")} /></SelectTrigger>',
    ].map((code) => ({ code, errors: [{ messageId: 'missing' }] })),
  });
});

test('text tokens keep their contrast', () => {
  tester.run('text-token-opacity', a11y.rules['text-token-opacity'], {
    valid: [
      '<p className="text-muted-foreground" />',
      '<p className="bg-muted-foreground/20" />',
    ],
    invalid: [
      '<p className="text-xs text-muted-foreground/60" />',
      'cn("hover:text-muted-foreground/80")',
      '<a className="text-xs text-primary/60" />',
      '<p className={`a ${b} text-muted-foreground/50`} />',
    ].map((code) => ({ code, errors: [{ messageId: 'dimmed' }] })),
  });
});
