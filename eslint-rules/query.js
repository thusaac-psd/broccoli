/**
 * `query.data ?? []` reads a query result as if a failed or unfinished
 * fetch were an empty answer. A query whose data is read with a fallback is
 * rendered through `QueryView` (`query={query}`) or `useQueryState(query)`,
 * which keep loading, failure and empty apart, or its failure is read
 * (`query.isError`); the fallback is then only what the view shows while the
 * state says why. A hook that returns the query leaves that to its caller.
 *
 * A query result is whatever has one's type (`data`, `isError`, `refetch`
 * and `fetchStatus`): a `useQuery` call, a hook that returns one
 * (`useRegistries()`), an element of `useQueries`, a prop, an alias or a
 * conditional choosing one. Without type information, only the `useQuery`
 * hooks by name. A fallback is `x ?? f`, `x || f`, or a conditional on the
 * read (`x ? g(x) : f`, `x == null ? f : x`), through any member chain
 * (`x?.items ?? []`), on a destructured or stored `data` (`const { data } =
 * q`, `const rows = q.data`) and on the `...rest` of a destructure. A default
 * that is deliberate is the query's own (`initialData`), never a fallback at
 * a read.
 */

const QUERY_HOOKS = /^use(Suspense)?(Infinite)?Query$/;
const RENDERERS = new Set(['useQueryState', 'queryState']);
/** The properties a query result has and a view state does not. */
const QUERY_SHAPE = ['data', 'isError', 'refetch', 'fetchStatus'];

/** Whether `node` hands the query to something that renders its state. */
function rendersState(node) {
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
 * Whether the value of `node` reaches anything: a condition, a render, a
 * call, a return, or a name that is read. `void x`, a bare `x;` and a name
 * no one reads drop it, so a state read there shows no failure.
 */
function valueUsed(context, node) {
  const parent = node.parent;
  if (parent.type === 'ExpressionStatement') return false;
  if (parent.type === 'UnaryExpression' && parent.operator === 'void') {
    return false;
  }
  if (parent.type === 'VariableDeclarator' && parent.init === node) {
    if (parent.id.type !== 'Identifier') return true;
    return context.sourceCode
      .getDeclaredVariables(parent)
      .some((v) => v.references.some((ref) => ref.isRead()));
  }
  return true;
}

/**
 * Whether `node` reads the query's state (`query.isError`) where its value
 * is used (`valueUsed`), or hands the query on (`return query`, `return {
 * query }`) to a caller that does.
 */
function handlesState(context, node) {
  const parent = node.parent;
  if (
    parent.type === 'MemberExpression' &&
    parent.object === node &&
    !parent.computed &&
    STATE.has(parent.property.name)
  ) {
    return valueUsed(context, chainTop(parent));
  }
  if (parent.type === 'ReturnStatement') return true;
  return (
    parent.type === 'Property' &&
    parent.value === node &&
    parent.parent.type === 'ObjectExpression' &&
    parent.parent.parent.type === 'ReturnStatement'
  );
}

/** Whether `node` calls a query hook by name (`useQuery(...)`). */
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

/** Whether `node` is `null` or `undefined`. */
function isNullish(node) {
  return (
    (node.type === 'Literal' && node.value === null) ||
    (node.type === 'Identifier' && node.name === 'undefined')
  );
}

/**
 * Whether `node`'s value, read through any member chain, falls back:
 * `x ?? []`, `x?.items || []`, `x ? f(x) : []`, `!x ? [] : x`,
 * `x === undefined ? [] : x`, `Array.isArray(x) ? x : []`, an early return
 * (`if (!x) return EMPTY`), and any of these after a nullish pass-through
 * (`(x ?? null) ?? []`). `x ?? null` alone substitutes nothing and is not
 * one, nor is an early return of nothing (`if (!x) return null`).
 */
function fallsBack(node) {
  let top = chainTop(node);
  // `(x ?? null) ?? []`: a nullish pass-through, then the fallback.
  while (
    top.parent?.type === 'LogicalExpression' &&
    (top.parent.operator === '??' || top.parent.operator === '||') &&
    top.parent.left === top &&
    isNullish(top.parent.right)
  ) {
    top = top.parent;
  }
  const parent = top.parent;
  if (
    parent?.type === 'LogicalExpression' &&
    (parent.operator === '??' || parent.operator === '||') &&
    parent.left === top
  ) {
    return true;
  }
  let test = top;
  // `Array.isArray(x)`, or any guard the read is handed to.
  if (parent?.type === 'CallExpression' && parent.arguments.includes(top)) {
    test = parent;
  } else if (
    parent?.type === 'BinaryExpression' &&
    ['==', '===', '!=', '!=='].includes(parent.operator) &&
    isNullish(parent.left === top ? parent.right : parent.left)
  ) {
    test = parent;
  }
  if (test.parent?.type === 'UnaryExpression' && test.parent.operator === '!') {
    test = test.parent;
  }
  const holder = test.parent;
  if (holder?.type === 'ConditionalExpression' && holder.test === test) {
    return true;
  }
  // `if (!x) return EMPTY`: an early return of anything but nothing.
  return (
    holder?.type === 'IfStatement' &&
    holder.test === test &&
    returnsSomething(holder.consequent)
  );
}

/** Whether `statement` returns a value other than null or undefined. */
function returnsSomething(statement) {
  const returned =
    statement.type === 'BlockStatement'
      ? statement.body.find((s) => s.type === 'ReturnStatement')
      : statement.type === 'ReturnStatement'
        ? statement
        : null;
  return !!returned?.argument && !isNullish(returned.argument);
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

/** `node` without the wrappers that do not change its value. */
function unwrap(node) {
  let at = node;
  while (at.type === 'TSNonNullExpression' || at.type === 'ChainExpression') {
    at = at.expression;
  }
  return at;
}

/** The identifier a member chain (`a.b[0]!`) starts from, if any. */
function rootOf(node) {
  let at = unwrap(node);
  while (at.type === 'MemberExpression') at = unwrap(at.object);
  return at.type === 'Identifier' ? at : null;
}

/** The variable `name` refers to from `node`. */
function variableOf(context, node) {
  let scope = context.sourceCode.getScope(node);
  for (; scope; scope = scope.upper) {
    const variable = scope.set.get(node.name);
    if (variable) return variable;
  }
  return null;
}

/** Whether `pattern` takes the query's state (`{ data, isError }`). */
function takesState(pattern) {
  return pattern.properties.some(
    (p) =>
      p.type === 'Property' &&
      p.key.type === 'Identifier' &&
      STATE.has(p.key.name),
  );
}

function create(context) {
  const services = context.sourceCode.parserServices;
  const checker = services?.program?.getTypeChecker();
  const reported = new Set();
  const report = (node) => {
    if (reported.has(node)) return;
    reported.add(node);
    context.report({ node, messageId: 'defaulted' });
  };
  const text = (node) =>
    context.sourceCode.getText(node).replace(/\?\./g, '.').replace(/!/g, '');

  /**
   * Whether the data `node` reads may be missing there: always without
   * types, and with them unless the code already narrowed it to present
   * (`q.data && q.data.total ?? 0` reads a field's own default, not a
   * fallback for the fetch).
   */
  function mayBeMissing(node) {
    if (!checker) return true;
    const type = services.getTypeAtLocation(node);
    return checker.getNonNullableType(type) !== type;
  }

  /** Whether `node` is a query result. */
  function isQuery(node) {
    if (!node) return false;
    if (isQueryCall(node)) return true;
    if (checker) {
      const type = services.getTypeAtLocation(node);
      return QUERY_SHAPE.every((p) => checker.getPropertyOfType(type, p));
    }
    // Without types: a name declared as a query hook's result, or the
    // `...rest` of a destructure of one that leaves its state in `rest`.
    if (node.type !== 'Identifier') return false;
    const def = variableOf(context, node)?.defs[0];
    if (def?.type !== 'Variable' || !isQueryCall(def.node.init)) return false;
    return (
      def.node.id === def.name ||
      (def.name.parent.type === 'RestElement' &&
        def.node.id.type === 'ObjectPattern' &&
        !takesState(def.node.id))
    );
  }

  /**
   * Whether the query `node` is has its state rendered or read anywhere
   * its variable is: `useQueryState(q)` beside `q.data ?? []`, and
   * `useQueryState(results[0])` beside `results[0].data ?? []`.
   */
  function stateHandled(node) {
    const root = rootOf(node);
    if (!root) return false;
    const variable = variableOf(context, root);
    if (!variable) return false;
    const want = text(node);
    return variable.references.some((ref) => {
      let at = ref.identifier;
      for (;;) {
        if (text(at) === want) {
          return rendersState(at) || handlesState(context, at);
        }
        const parent = at.parent;
        if (
          (parent?.type === 'MemberExpression' && parent.object === at) ||
          ((parent?.type === 'TSNonNullExpression' ||
            parent?.type === 'ChainExpression') &&
            parent.expression === at)
        ) {
          at = parent;
        } else {
          return false;
        }
      }
    });
  }

  /** The variables `pattern`'s declarator or function declares. */
  function declaredIn(pattern) {
    let owner = pattern;
    while (
      owner &&
      owner.type !== 'VariableDeclarator' &&
      !owner.type.includes('Function')
    ) {
      owner = owner.parent;
    }
    return owner ? context.sourceCode.getDeclaredVariables(owner) : [];
  }

  /** Reports each read of `identifier`'s variable that falls back. */
  function checkReads(identifier) {
    const variable = declaredIn(identifier).find((v) =>
      v.identifiers.includes(identifier),
    );
    for (const ref of variable?.references ?? []) {
      if (
        ref.identifier !== identifier &&
        fallsBack(ref.identifier) &&
        mayBeMissing(ref.identifier)
      ) {
        report(ref.identifier);
      }
    }
  }

  /**
   * `{ data: x, ...rest } = <query>`: the `data` binding reported where it
   * is read with a fallback, or the default it is given, unless the pattern
   * also takes the query's state and reads it.
   */
  function checkPattern(pattern) {
    if (takesUsedState(pattern)) return;
    for (const property of pattern.properties) {
      if (
        property.type !== 'Property' ||
        property.key.type !== 'Identifier' ||
        property.key.name !== 'data'
      ) {
        continue;
      }
      if (property.value.type === 'AssignmentPattern') {
        report(property.value);
      } else if (property.value.type === 'Identifier') {
        checkReads(property.value);
      }
    }
  }

  /** Whether `pattern` takes the query's state into a name that is read. */
  function takesUsedState(pattern) {
    return pattern.properties.some((p) => {
      if (
        p.type !== 'Property' ||
        p.key.type !== 'Identifier' ||
        !STATE.has(p.key.name)
      ) {
        return false;
      }
      if (p.value.type !== 'Identifier') return true;
      const variable = declaredIn(p.value).find((v) =>
        v.identifiers.includes(p.value),
      );
      return !!variable?.references.some((ref) => ref.isRead());
    });
  }

  return {
    // A destructure of a query: `const { data } = q`, `[{ data }] = qs`,
    // a component's `{ query: { data } }`.
    ObjectPattern(node) {
      const declarator = node.parent;
      const value =
        declarator.type === 'VariableDeclarator' && declarator.id === node
          ? declarator.init
          : node;
      if (!isQuery(value)) return;
      if (value !== node && value.type === 'Identifier' && stateHandled(value))
        return;
      checkPattern(node);
    },
    // `const rows = q.data`: an alias of the data, also through a nullish
    // pass-through, is read as the data.
    VariableDeclarator(node) {
      if (node.id.type !== 'Identifier' || !node.init) return;
      // `q.data ?? null` passes the data through as it is.
      let init = unwrap(node.init);
      while (
        init.type === 'LogicalExpression' &&
        init.operator === '??' &&
        isNullish(init.right)
      ) {
        init = unwrap(init.left);
      }
      if (
        init.type !== 'MemberExpression' ||
        dataOf(init.object) !== init ||
        !isQuery(init.object) ||
        stateHandled(init.object)
      ) {
        return;
      }
      checkReads(node.id);
    },
    // `q.data ?? []`, `useQuery(o).data ?? []`, `results[0].data ?? []`.
    MemberExpression(node) {
      if (
        dataOf(node.object) !== node ||
        !fallsBack(node) ||
        !mayBeMissing(node) ||
        !isQuery(node.object) ||
        stateHandled(node.object)
      ) {
        return;
      }
      report(node);
    },
  };
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
  create,
};

export default {
  rules: {
    'no-defaulted-query-data': noDefaultedQueryData,
  },
};
