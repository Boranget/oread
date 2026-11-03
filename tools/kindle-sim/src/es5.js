// ES5 语法门禁：用 acorn 以 ecmaVersion 5 解析，并统计 ES6+ 语法出现次数。
// 这里刻意不 depend on 浏览器：解析结果与 KPW3 的 WebKit 解析期失败语义对应
// （解析期失败 => 整个外部脚本不会执行）。
import * as acorn from 'acorn';

const FEATURE_KEYS = [
  'arrow', 'let', 'const', 'template', 'promise', 'class', 'async', 'await',
  'forOf', 'spread', 'module', 'optionalChaining', 'nullish', 'exponent',
];

function emptyFeatures() {
  const out = {};
  for (const k of FEATURE_KEYS) out[k] = 0;
  return out;
}

/**
 * 以 ES5 严格语义解析脚本。
 * @returns {{ok:boolean, error:null|{message:string,line:number,column:number,position:number}, features:object}}
 */
export function parseAsES5(code, url = '') {
  let ok = true;
  let error = null;
  try {
    acorn.parse(code, {
      ecmaVersion: 5,
      sourceType: 'script',
      allowHashBang: false,
      locations: true,
    });
  } catch (e) {
    ok = false;
    const loc = e.loc || {};
    error = {
      // acorn 的 message 末尾自带 "(line:col)"；这里去掉，报告统一用 line/column 字段，
      // 避免出现 “(1:36) (1:37)” 这类重复。
      message: (e.message || String(e)).replace(/\s*\(\d+:\d+\)\s*$/, ''),
      line: loc.line || 0,
      // acorn 的 column 从 0 开始；报告里统一按 1 开始显示
      column: typeof loc.column === 'number' ? loc.column + 1 : 0,
      position: typeof e.pos === 'number' ? e.pos : -1,
      sourceUrl: url,
    };
  }
  return { ok, error, features: countFeatures(code) };
}

/**
 * 用最新语法解析并统计 ES6+ 结构；若最新解析也失败，退回正则粗扫。
 */
export function countFeatures(code) {
  const features = emptyFeatures();
  let ast = null;
  try {
    ast = acorn.parse(code, { ecmaVersion: 'latest', sourceType: 'script', locations: false });
  } catch (_) {
    try {
      ast = acorn.parse(code, { ecmaVersion: 'latest', sourceType: 'module', locations: false });
    } catch (_e) {
      return regexCount(code, features);
    }
  }
  walk(ast, (node) => {
    switch (node.type) {
      case 'ArrowFunctionExpression':
        features.arrow += 1;
        break;
      case 'VariableDeclaration':
        if (node.kind === 'let') features.let += 1;
        if (node.kind === 'const') features.const += 1;
        break;
      case 'TemplateLiteral':
        features.template += 1;
        break;
      case 'Identifier':
        if (node.name === 'Promise') features.promise += 1;
        break;
      case 'ClassDeclaration':
      case 'ClassExpression':
        features.class += 1;
        break;
      case 'FunctionDeclaration':
      case 'FunctionExpression':
      case 'ArrowFunctionExpression':
      case 'ObjectMethod':
      case 'ClassMethod':
        if (node.async) features.async += 1;
        break;
      case 'AwaitExpression':
        features.await += 1;
        break;
      case 'ForOfStatement':
        features.forOf += 1;
        break;
      case 'SpreadElement':
        features.spread += 1;
        break;
      case 'ImportDeclaration':
      case 'ExportNamedDeclaration':
      case 'ExportDefaultDeclaration':
      case 'ExportAllDeclaration':
        features.module += 1;
        break;
      case 'ChainExpression':
        features.optionalChaining += 1;
        break;
      case 'LogicalExpression':
        if (node.operator === '??') features.nullish += 1;
        break;
      case 'BinaryExpression':
        if (node.operator === '**') features.exponent += 1;
        break;
      default:
        break;
    }
  });
  return features;
}

function walk(root, visit) {
  const stack = [root];
  while (stack.length) {
    const node = stack.pop();
    if (!node || typeof node !== 'object') continue;
    if (typeof node.type === 'string') visit(node);
    for (const key of Object.keys(node)) {
      if (key === 'loc' || key === 'start' || key === 'end' || key === 'range') continue;
      const value = node[key];
      if (Array.isArray(value)) {
        for (let i = value.length - 1; i >= 0; i -= 1) {
          if (value[i] && typeof value[i] === 'object') stack.push(value[i]);
        }
      } else if (value && typeof value === 'object') {
        stack.push(value);
      }
    }
  }
}

function regexCount(code, features) {
  const clean = code.replace(/\/\*[\s\S]*?\*\//g, ' ');
  features.arrow = (clean.match(/=>/g) || []).length;
  features.let = (clean.match(/\blet\s+[A-Za-z_$[]/g) || []).length;
  features.const = (clean.match(/\bconst\s+[A-Za-z_$[]/g) || []).length;
  features.template = (clean.match(/`/g) || []).length;
  features.promise = (clean.match(/\bPromise\b/g) || []).length;
  features.class = (clean.match(/\bclass\s+[A-Za-z_$]/g) || []).length;
  features.async = (clean.match(/\basync\s+(?:function|\()/g) || []).length;
  features.await = (clean.match(/\bawait\b/g) || []).length;
  features.forOf = (clean.match(/\bfor\s*\(\s*(?:var|let|const)?[^;]*\bof\b/g) || []).length;
  features.spread = (clean.match(/\.\.\./g) || []).length;
  features.module = (clean.match(/\b(?:import|export)\s/g) || []).length;
  features.optionalChaining = (clean.match(/\?\./g) || []).length;
  features.nullish = (clean.match(/\?\?/g) || []).length;
  features.exponent = (clean.match(/\*\*/g) || []).length;
  return features;
}

export function featureSummary(features) {
  if (!features) return '';
  return [
    ['箭头', features.arrow],
    ['let', features.let],
    ['const', features.const],
    ['模板串', features.template],
    ['Promise', features.promise],
    ['class', features.class],
    ['async', features.async],
    ['await', features.await],
    ['for-of', features.forOf],
    ['spread', features.spread],
    ['module', features.module],
    ['?.', features.optionalChaining],
    ['??', features.nullish],
    ['**', features.exponent],
  ]
    .filter(([, n]) => n > 0)
    .map(([k, n]) => `${k}×${n}`)
    .join('、') || '无';
}

/**
 * 分析一个脚本（外部或内联）。
 */
export function analyzeScript(code, url, kind = 'external') {
  const result = parseAsES5(code, url);
  return {
    url,
    kind,
    bytes: Buffer.byteLength(code, 'utf8'),
    parseable: result.ok,
    error: result.error,
    features: result.features,
    featureSummary: featureSummary(result.features),
  };
}

export { FEATURE_KEYS };
