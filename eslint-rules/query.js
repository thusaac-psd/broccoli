/**
 * `query.data ?? []` reads a `useQuery` result as if a failed or unfinished
 * fetch were an empty answer. A query whose data is read with a fallback is
 * rendered through `QueryView` (`query={query}`) or `useQueryState(query)`,
 * which keep loading, failure and empty apart, or its failure is read
 * (`query.isError`); the fallback is then only what the view shows while the
 * state says why. A hook that returns the query leaves that to its caller.
 * A destructured query (`const { data: x } = useQuery(...)`, a second
 * destructure of a stored one, or the `...rest` of either) is held to the
 * same rule, and so is a read through a member chain (`x?.items ?? []`)
 * and a query no name holds (`useQuery(...).data ?? []`). A default that is
 * deliberate is the query's own (`initialData`), never a fallback at a read.
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
  // `useQueryState({ ...query, data: rows })`: the query, reshaped.
  if (
    parent.type === 'SpreadElement' &&
    parent.parent.type === 'ObjectExpression' &&
    parent.parent.parent.type === 'CallExpression' &&
    parent.parent.parent.callee.type === 'Identifier' &&
    RENDERERS.has(parent.parent.parent.callee.name)
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

/** Whether `node` calls a query hook (`useQuery(...)`). */
function isQueryCall(node) {
  return (
    node?.type === 'CallExpression' &&
    node.callee.type === 'Identifier' &&
    QUERY_HOOKS.test(node.callee.name)
  );
}

/** The whole member chain `node` starts: `x` of `x.a?.b!` climbs to it. */
function chainTop(node) {
  let at = node;
  for (;;) {
    const parent = at.parent;
    if (parent?.type === 'MemberExpression' && parent.object === at) {
      at = parent;
    } else if (
      (parent?.type === 'ChainExpression' ||
        parent?.type === 'TSNonNullExpression') &&
      parent.expression === at
    ) {
      at = parent;
    } else {
      return at;
    }
  }
}

/**
 * Whether `node`'s value, read through any member chain, falls back:
 * `x ?? []`, `x?.items || []`.
 */
function fallsBack(node) {
  const top = chainTop(node);
  const logical = top.parent;
  return (
    logical?.type === 'LogicalExpression' &&
    (logical.operator === '??' || logical.operator === '||') &&
    logical.left === top
  );
}

/** The `x.data` read whose object is `node`, if it is one. */
function dataOf(node) {
  const member = node.parent;
  return member?.type === 'MemberExpression' &&
    member.object === node &&
    !member.computed &&
    member.property.name === 'data'
    ? member
    : null;
}

/** The variable `pattern` (of `declarator`) declares as `name`. */
function declared(context, declarator, name) {
  return context.sourceCode
    .getDeclaredVariables(declarator)
    .find((variable) => variable.name === name);
}

/**
 * A query held whole in `variable` (`const q = useQuery(...)`, or the rest
 * of a destructured one): its `data` read with a fallback is reported,
 * through a second destructure too, unless the query's state is rendered or
 * read beside it.
 */
function checkQuery(context, variable) {
  if (
    !variable ||
    variable.references.some((ref) => rendersState(ref) || handlesState(ref))
  ) {
    return;
  }
  for (const ref of variable.references) {
    const data = dataOf(ref.identifier);
    if (data && fallsBack(data)) {
      context.report({ node: data, messageId: 'defaulted' });
    }
    const declarator = ref.identifier.parent;
    if (
      declarator.type === 'VariableDeclarator' &&
      declarator.init === ref.identifier &&
      declarator.id.type === 'ObjectPattern'
    ) {
      checkPattern(context, declarator);
    }
  }
}

/**
 * `const { data: x, ...rest } = <query>`: the `data` binding reported where
 * it is read with a fallback, or the default it is given, and `rest` held
 * as the query, unless the pattern also takes the query's state.
 */
function checkPattern(context, declarator) {
  const properties = declarator.id.properties;
  const named = (p) =>
    p.type === 'Property' && p.key.type === 'Identifier' ? p.key.name : null;
  if (properties.some((p) => STATE.has(named(p)))) return;
  for (const property of properties) {
    if (property.type === 'RestElement') {
      if (property.argument.type === 'Identifier') {
        checkQuery(
          context,
          declared(context, declarator, property.argument.name),
        );
      }
      continue;
    }
    if (named(property) !== 'data') continue;
    if (property.value.type === 'AssignmentPattern') {
      context.report({ node: property.value, messageId: 'defaulted' });
      continue;
    }
    if (property.value.type !== 'Identifier') continue;
    const variable = declared(context, declarator, property.value.name);
    for (const ref of variable?.references ?? []) {
      if (fallsBack(ref.identifier)) {
        context.report({ node: ref.identifier, messageId: 'defaulted' });
      }
    }
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
        if (!isQueryCall(node.init)) return;
        if (node.id.type === 'ObjectPattern') {
          checkPattern(context, node);
        } else if (node.id.type === 'Identifier') {
          checkQuery(context, declared(context, node, node.id.name));
        }
      },
      // `useQuery(...).data ?? []`: a query no one can ask about.
      MemberExpression(node) {
        if (
          isQueryCall(node.object) &&
          dataOf(node.object) === node &&
          fallsBack(node)
        ) {
          context.report({ node, messageId: 'defaulted' });
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
