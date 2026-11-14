/**
 * A button (or link) whose content is only icons has no accessible name, so
 * a screen reader announces just "button". Such a control needs an
 * `aria-label` (or `aria-labelledby`); `<Button size="icon">` requires one at
 * the type level, and this rule catches the rest: native `<button>`/`<a>`,
 * `<Link>`, and `<Button asChild>` wrappers.
 *
 * Content counts as text when it holds JSX text, an image with `alt`, or an
 * expression that is not just icons (`{cond ? <A /> : <B />}`, `{x && <A />}`
 * are icons; `{t('key')}` is text).
 */

const CONTROLS = new Set(['Button', 'button', 'a', 'Link', 'NavLink']);
const LABEL_ATTRS = new Set(['aria-label', 'aria-labelledby']);

function elementName(node) {
  const name = node.openingElement.name;
  return name.type === 'JSXIdentifier' ? name.name : null;
}

function attribute(node, name) {
  return node.openingElement.attributes.find(
    (attr) => attr.type === 'JSXAttribute' && attr.name.name === name,
  );
}

function hasLabelAttribute(node) {
  return node.openingElement.attributes.some(
    (attr) =>
      // A spread may carry the label (a wrapper forwarding its props).
      attr.type === 'JSXSpreadAttribute' ||
      (attr.type === 'JSXAttribute' && LABEL_ATTRS.has(attr.name.name)),
  );
}

/** An `alt` that is not empty: an image that names its control. */
function hasAlt(node) {
  const alt = attribute(node, 'alt');
  if (!alt?.value) return false;
  return !(alt.value.type === 'Literal' && alt.value.value.trim() === '');
}

function isTextExpression(expr) {
  switch (expr.type) {
    case 'JSXEmptyExpression':
      return false;
    case 'JSXElement':
    case 'JSXFragment':
      return hasText(expr);
    case 'ConditionalExpression':
      return (
        isTextExpression(expr.consequent) || isTextExpression(expr.alternate)
      );
    case 'LogicalExpression':
      return isTextExpression(expr.right);
    case 'Literal':
      return typeof expr.value === 'string' && expr.value.trim() !== '';
    case 'Identifier':
      return expr.name !== 'undefined';
    default:
      return true;
  }
}

function hasText(node) {
  return node.children.some((child) => {
    switch (child.type) {
      case 'JSXText':
        return child.value.trim() !== '';
      case 'JSXExpressionContainer':
        return isTextExpression(child.expression);
      case 'JSXElement':
        return (
          hasLabelAttribute(child) ||
          (elementName(child) === 'img' && hasAlt(child)) ||
          hasText(child)
        );
      case 'JSXFragment':
        return hasText(child);
      default:
        return true;
    }
  });
}

/** @type {import('eslint').Rule.RuleModule} */
const iconButtonLabel = {
  meta: {
    type: 'problem',
    docs: { description: 'Require an accessible name on icon-only controls' },
    messages: {
      missing:
        'This control shows only an icon: give it an aria-label (for <Button>, size="icon" with aria-label).',
    },
    schema: [],
  },
  create(context) {
    return {
      JSXElement(node) {
        const name = elementName(node);
        if (!name || !CONTROLS.has(name)) return;
        if (node.openingElement.selfClosing) return;
        if (hasLabelAttribute(node)) return;
        // `<Button asChild><Link>…</Link></Button>`: the child is the
        // control and is checked on its own.
        if (attribute(node, 'asChild')) return;
        if (hasText(node)) return;
        context.report({ node: node.openingElement, messageId: 'missing' });
      },
    };
  },
};

/**
 * An image without `alt` is announced by its file name. A decorative image
 * says so with `alt=""`.
 *
 * @type {import('eslint').Rule.RuleModule}
 */
const imgAlt = {
  meta: {
    type: 'problem',
    docs: { description: 'Require alt on images' },
    messages: {
      missing: 'Give this image an alt text, or alt="" if it is decorative.',
    },
    schema: [],
  },
  create(context) {
    return {
      JSXOpeningElement(node) {
        if (node.name.type !== 'JSXIdentifier' || node.name.name !== 'img')
          return;
        const labelled = node.attributes.some(
          (attr) =>
            attr.type === 'JSXSpreadAttribute' ||
            attr.name.name === 'alt' ||
            attr.name.name === 'aria-hidden',
        );
        if (!labelled) context.report({ node, messageId: 'missing' });
      },
    };
  },
};

/**
 * A `<select>` shows its current option, not what it chooses, so it needs a
 * name: an `aria-label`, `aria-labelledby`, or an `id` a `<Label htmlFor>`
 * points at.
 *
 * @type {import('eslint').Rule.RuleModule}
 */
const selectLabel = {
  meta: {
    type: 'problem',
    docs: { description: 'Require a label on select elements' },
    messages: {
      missing:
        'Name this select: aria-label, aria-labelledby, or an id with a <Label htmlFor>.',
    },
    schema: [],
  },
  create(context) {
    return {
      JSXOpeningElement(node) {
        if (node.name.type !== 'JSXIdentifier' || node.name.name !== 'select')
          return;
        const labelled = node.attributes.some(
          (attr) =>
            attr.type === 'JSXSpreadAttribute' ||
            ['id', ...LABEL_ATTRS].includes(attr.name.name),
        );
        if (!labelled) context.report({ node, messageId: 'missing' });
      },
    };
  },
};

/** Accessibility rules for the web app, the SDK and plugin frontends. */
export default {
  rules: {
    'icon-button-label': iconButtonLabel,
    'img-alt': imgAlt,
    'select-label': selectLabel,
  },
};
