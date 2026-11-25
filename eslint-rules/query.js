/**
 * `query.data ?? []` reads a `useQuery` result as if a failed or unfinished
 * fetch were an empty answer. A query whose data is read with a fallback is
 * rendered through `QueryView` (`query={query}`) or `useQueryState(query)`,
 * which keep loading, failure and empty apart, or its failure is read
 * (`query.isError`); the fallback is then only what the view shows while the
 * state says why. A hook that returns the query leaves that to its caller.
 * A destructured query (`const { data: x } = useQuery(...)`) is held to the
 * same rule: its `data` read with a fallback, or given a default, needs the
 * query's state taken beside it.
 */

const QUERY_HOOKS = /^use(Suspense)?(Infinite)?Query$/;
const RENDERERS = new Set(['useQueryState', 'queryState']);

/** Whether `ref` hands the query to something that renders its state. */
function rendersState(ref) {
  const node = ref.identifier;
  const parent = node.parent;
  if (
    parent.type === 'CallExpression' &&
    parent.arguments.includes(node) &&
    parent.callee.type === 'Identifier' &&
    RENDERERS.has(parent.callee.name)
  ) {
    return true;
  }
  // `<QueryView query={query}>`, or a conditional choosing the query.
  let at = node;
  while (at.parent?.type === 'ConditionalExpression') at = at.parent;
  if (
    at.parent?.type === 'CallExpression' &&
    at.parent.callee.type === 'Identifier' &&
    RENDERERS.has(at.parent.callee.name)
  ) {
    return true;
  }
  return (
    at.parent?.type === 'JSXExpressionContainer' &&
    at.parent.parent?.type === 'JSXAttribute' &&
    at.parent.parent.name.name === 'query'
  );
}

/** Reading these says what became of the fetch. */
const STATE = new Set([
  'error',
  'isError',
  'status',
  'isSuccess',
  'failureReason',
]);

/**
 * Whether `ref` reads the query's state (`query.isError`), or hands the
 * query on (`return query`, `return { query }`) to a caller that does.
 */
function handlesState(ref) {
  const node = ref.identifier;
  const parent = node.parent;
  if (
    parent.type === 'MemberExpression' &&
    parent.object === node &&
    !parent.computed &&
    STATE.has(parent.property.name)
  ) {
    return true;
  }
  if (parent.type === 'ReturnStatement') return true;
  return (
    parent.type === 'Property' &&
    parent.value === node &&
    parent.parent.type === 'ObjectExpression' &&
    parent.parent.parent.type === 'ReturnStatement'
  );
}

/** The `x.data` of `x.data ?? fallback` (or `||`), if `ref` is its `x`. */
function defaultedData(ref) {
  const member = ref.identifier.parent;
  if (
    member.type !== 'MemberExpression' ||
    member.object !== ref.identifier ||
    member.computed ||
    member.property.name !== 'data'
  ) {
    return null;
  }
  const logical = member.parent;
  return logical.type === 'LogicalExpression' &&
    (logical.operator === '??' || logical.operator === '||') &&
    logical.left === member
    ? member
    : null;
}

/** Whether `ref` reads its value with a fallback: `x ?? fallback` or `x || fallback`. */
function fallsBack(ref) {
  const logical = ref.identifier.parent;
  return (
    logical.type === 'LogicalExpression' &&
    (logical.operator === '??' || logical.operator === '||') &&
    logical.left === ref.identifier
  );
}

/**
 * `const { data: x } = useQuery(...)`, renamed or not: the `data` binding
 * reported where it is read with a fallback, or the default it is given,
 * unless the pattern also takes the query's state (`error`, `isError`, ...).
 */
function checkDestructured(context, node) {
  const fields = node.id.properties.filter((p) => p.type === 'Property');
  const named = (p) => (p.key.type === 'Identifier' ? p.key.name : null);
  if (fields.some((p) => STATE.has(named(p)))) return;
  const data = fields.find((p) => named(p) === 'data');
  if (!data) return;
  if (data.value.type === 'AssignmentPattern') {
    context.report({ node: data.value, messageId: 'defaulted' });
    return;
  }
  if (data.value.type !== 'Identifier') return;
  const variable = context.sourceCode
    .getDeclaredVariables(node)
    .find((v) => v.name === data.value.name);
  for (const ref of variable?.references ?? []) {
    if (fallsBack(ref))
      context.report({ node: ref.identifier, messageId: 'defaulted' });
  }
}

const noDefaultedQueryData = {
  meta: {
    type: 'problem',
    messages: {
      defaulted:
        'A query read with a fallback shows a failed fetch as that fallback: render it through QueryView or useQueryState().',
    },
    schema: [],
  },
  create(context) {
    return {
      VariableDeclarator(node) {
        if (
          node.id.type === 'ObjectPattern' &&
          node.init?.type === 'CallExpression' &&
          node.init.callee.type === 'Identifier' &&
          QUERY_HOOKS.test(node.init.callee.name)
        ) {
          checkDestructured(context, node);
          return;
        }
        if (
          node.id.type !== 'Identifier' ||
          node.init?.type !== 'CallExpression' ||
          node.init.callee.type !== 'Identifier' ||
          !QUERY_HOOKS.test(node.init.callee.name)
        ) {
          return;
        }
        const [variable] = context.sourceCode.getDeclaredVariables(node);
        if (
          !variable ||
          variable.references.some(
            (ref) => rendersState(ref) || handlesState(ref),
          )
        ) {
          return;
        }
        for (const ref of variable.references) {
          const member = defaultedData(ref);
          if (member) context.report({ node: member, messageId: 'defaulted' });
        }
      },
    };
  },
};

export default {
  rules: {
    'no-defaulted-query-data': noDefaultedQueryData,
  },
};
