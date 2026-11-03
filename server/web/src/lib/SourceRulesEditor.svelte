<script>
  // 书源字段级编辑：基础信息 + 五段规则（搜索/详情/目录/正文/图片）。
  // 规则值保持原始字符串，按 @css: / $ / // / ## / @js: 识别模式；
  // 保存时只重写这五段，其余字段（发现、评论、header、weight…）原样保留。
  import { createEventDispatcher } from 'svelte';

  export let source = {};
  export let saving = false;
  export let validating = false;
  export let validation = null;
  export let saveError = '';

  const dispatch = createEventDispatcher();

  // ------------------------------------------------------------------
  // 规则段与字段（左中文、右原字段名）
  // ------------------------------------------------------------------
  const SECTIONS = [
    {
      id: 'search',
      ruleKey: 'rule_search',
      title: '搜索规则',
      en: 'ruleSearch',
      open: true,
      fields: [
        ['check_key_word', '检查关键词'],
        ['book_list', '书籍列表'],
        ['name', '书名'],
        ['author', '作者'],
        ['intro', '简介'],
        ['kind', '分类'],
        ['last_chapter', '最新章节'],
        ['update_time', '更新时间'],
        ['book_url', '书籍链接'],
        ['cover_url', '封面链接'],
        ['word_count', '字数'],
      ],
    },
    {
      id: 'book',
      ruleKey: 'rule_book_info',
      title: '详情规则',
      en: 'ruleBookInfo',
      fields: [
        ['init', '初始化'],
        ['name', '书名'],
        ['author', '作者'],
        ['intro', '简介'],
        ['kind', '分类'],
        ['last_chapter', '最新章节'],
        ['update_time', '更新时间'],
        ['cover_url', '封面链接'],
        ['toc_url', '目录链接'],
        ['word_count', '字数'],
        ['can_re_name', '允许改名'],
        ['download_urls', '下载链接'],
      ],
    },
    {
      id: 'toc',
      ruleKey: 'rule_toc',
      title: '目录规则',
      en: 'ruleToc',
      fields: [
        ['pre_update_js', '目录更新前脚本'],
        ['chapter_list', '章节列表'],
        ['chapter_name', '章节名'],
        ['chapter_url', '章节链接'],
        ['format_js', '格式化脚本'],
        ['is_volume', '卷标识'],
        ['is_vip', 'VIP 标识'],
        ['is_pay', '付费标识'],
        ['update_time', '更新时间'],
        ['next_toc_url', '下一页目录'],
      ],
    },
    {
      id: 'content',
      ruleKey: 'rule_content',
      title: '正文规则',
      en: 'ruleContent',
      fields: [
        ['content', '正文'],
        ['title', '章节标题'],
        ['next_content_url', '下一页正文'],
        ['web_js', '网页脚本'],
        ['source_regex', '源码正则'],
        ['replace_regex', '替换正则'],
        ['replace_summary', '替换摘要'],
        ['pay_action', '付费操作'],
      ],
    },
    {
      id: 'images',
      ruleKey: 'rule_content',
      title: '图片规则',
      en: 'ruleContent（图片字段）',
      imageOnly: true,
      fields: [
        ['image_style', '图片样式'],
        ['image_decode', '图片解码'],
      ],
    },
  ];

  const MODES = [
    { id: 'css', label: 'CSS', prefix: '@css:' },
    { id: 'json', label: 'JsonPath', prefix: '$' },
    { id: 'xpath', label: 'XPath', prefix: '//' },
    { id: 'regex', label: 'Regex', prefix: '##' },
    { id: 'js', label: 'JS', prefix: '@js:' },
  ];

  const EXTRA_LABELS = {
    replace_summary: '替换摘要',
    replaceSummary: '替换摘要',
    checkKeyWord: '检查关键词',
    bookList: '书籍列表',
    coverUrl: '封面链接',
    bookUrl: '书籍链接',
    lastChapter: '最新章节',
    updateTime: '更新时间',
    wordCount: '字数',
    tocUrl: '目录链接',
    preUpdateJs: '目录更新前脚本',
    chapterList: '章节列表',
    chapterName: '章节名',
    chapterUrl: '章节链接',
    formatJs: '格式化脚本',
    isVolume: '卷标识',
    isVip: 'VIP 标识',
    isPay: '付费标识',
    nextTocUrl: '下一页目录',
    nextContentUrl: '下一页正文',
    webJs: '网页脚本',
    sourceRegex: '源码正则',
    replaceRegex: '替换正则',
    payAction: '付费操作',
    canReName: '允许改名',
    downloadUrls: '下载链接',
  };

  const BASE_KEYS = {
    book_source_name: 'bookSourceName',
    book_source_url: 'bookSourceUrl',
    book_source_group: 'bookSourceGroup',
    book_source_type: 'bookSourceType',
    enabled: 'enabled',
    search_url: 'searchUrl',
    book_source_comment: 'bookSourceComment',
  };

  const RULE_KEYS = [
    ['rule_search', 'ruleSearch'],
    ['rule_book_info', 'ruleBookInfo'],
    ['rule_toc', 'ruleToc'],
    ['rule_content', 'ruleContent'],
  ];

  const RULE_META = [
    { ruleKey: 'rule_search', secIds: ['search'] },
    { ruleKey: 'rule_book_info', secIds: ['book'] },
    { ruleKey: 'rule_toc', secIds: ['toc'] },
    { ruleKey: 'rule_content', secIds: ['content', 'images'] },
  ];

  const clone = (v) => JSON.parse(JSON.stringify(v ?? {}));
  const snakeToCamel = (k) => k.replace(/_([a-z])/g, (_, c) => c.toUpperCase());

  // 编辑态：base 保留未展示的字段；values 保存五段规则的字符串；extraKeys 记录原 JSON 里多出的字段
  let base = clone(source);
  let values = {};
  let extraKeys = {};
  let modes = {};
  let ruleVersion = 0;
  let sections = [];
  let openState = { search: true, book: false, toc: false, content: false, images: false };

  let localErrors = {};
  let localBlocked = 0;

  let pasteOpen = false;
  let pasteText = '';
  let pasteError = '';
  let pasteNotice = '';

  let lastValidation = null;
  let backendErrors = {};

  function sectionById(id) {
    return SECTIONS.find((s) => s.id === id);
  }

  function knownKeys(sec) {
    return sec.fields.map(([k]) => k);
  }

  function fieldId(secId, key) {
    return `${secId}.${key}`;
  }

  function hydrate(src) {
    base = clone(src);
    // select 的 value 是字符串，数字类型先归一，避免绑定时选不中任何 option
    base.book_source_type = String(base.book_source_type ?? 0);
    values = {};
    extraKeys = {};
    modes = {};
    SECTIONS.forEach((sec) => {
      const obj = base[sec.ruleKey] && typeof base[sec.ruleKey] === 'object' ? base[sec.ruleKey] : {};
      const known = knownKeys(sec);
      sec.fields.forEach(([key]) => {
        const present = obj[key] !== undefined && obj[key] !== null;
        if (sec.imageOnly && !present) return;
        values[fieldId(sec.id, key)] = present ? String(obj[key]) : '';
      });
      if (!sec.imageOnly) {
        Object.keys(obj).forEach((key) => {
          if (known.includes(key)) return;
          if (isImageField(key)) return; // 图片字段单独成块
          (extraKeys[sec.id] || (extraKeys[sec.id] = [])).push(key);
          values[fieldId(sec.id, key)] = obj[key] == null ? '' : String(obj[key]);
        });
      }
    });
    ruleVersion += 1;
  }

  function isImageField(key) {
    return key === 'image_style' || key === 'image_decode';
  }

  function buildSections() {
    const out = SECTIONS.map((sec) => {
      const rows = [];
      sec.fields.forEach(([key, label]) => {
        if (sec.imageOnly && !(values[fieldId(sec.id, key)] || '').trim()) return;
        rows.push({ key, label });
      });
      (extraKeys[sec.id] || []).forEach((key) => {
        rows.push({ key, label: EXTRA_LABELS[key] || '' });
      });
      return { ...sec, rows };
    });
    // 图片块：两个字段都不存在时整块隐藏
    return out.filter((sec) => !sec.imageOnly || sec.rows.length > 0);
  }

  hydrate(base);

  $: sections = buildSections(ruleVersion);

  $: typeNum = Number(base.book_source_type ?? 0);
  $: typeSupported = typeNum === 0 || typeNum === 2;

  $: backendErrors = mapBackendErrors(validation);
  $: shownErrors = { ...backendErrors, ...localErrors };
  $: generalErrors = backendErrors._general || [];
  $: warnings = (validation && validation.warnings) || [];

  $: if (validation && validation !== lastValidation) {
    lastValidation = validation;
    const next = { ...openState };
    Object.keys(shownErrors).forEach((key) => {
      const secId = key.split('.')[0];
      if (secId !== 'base') next[secId] = true;
    });
    openState = next;
  }

  // ------------------------------------------------------------------
  // 模式识别与前后缀
  // ------------------------------------------------------------------
  function detectMode(value) {
    const s = String(value || '').trim();
    if (/^@js\s*:/i.test(s) || /^<js>/i.test(s)) return 'js';
    if (/^@css\s*:/i.test(s)) return 'css';
    if (/^@(xpath|xml)\s*:/i.test(s) || s.startsWith('//')) return 'xpath';
    if (/^@(json|jsonpath)\s*:/i.test(s) || s.startsWith('$')) return 'json';
    if (/^@(regex|regexp)\s*:/i.test(s) || s.startsWith('##')) return 'regex';
    return 'css';
  }

  function stripPrefix(value) {
    let s = String(value || '').trim();
    const m = /^@(css|xpath|xml|json|jsonpath|js|regex|regexp)\s*:/i.exec(s);
    if (m) {
      s = s.slice(m[0].length).trim();
    } else {
      if (s.startsWith('//')) return s.slice(2).trim();
      if (s.startsWith('##')) return s.slice(2).trim();
    }
    if (s.startsWith('$')) {
      const rest = s.slice(1).trim();
      // $.bookList -> bookList；$..bookList 保留递归写法
      if (rest.startsWith('.') && !rest.startsWith('..')) return rest.slice(1).trim();
      return rest;
    }
    return s;
  }

  function applyPrefix(mode, core) {
    if (!core) return '';
    if (mode === 'css') return '@css:' + core;
    if (mode === 'js') return '@js:' + core;
    if (mode === 'xpath') return core.startsWith('/') ? core : '//' + core;
    if (mode === 'json') return /^[.[]/.test(core) ? '$' + core : '$.' + core;
    if (mode === 'regex') return '##' + core;
    return core;
  }

  function modeFor(sec, key, modeMap) {
    const id = fieldId(sec.id, key);
    const raw = values[id];
    if (raw && raw.trim()) return detectMode(raw);
    return modeMap[id] || 'css';
  }

  function changeMode(sec, key, event) {
    const next = event.currentTarget.value;
    const id = fieldId(sec.id, key);
    const core = stripPrefix(values[id] || '');
    values[id] = applyPrefix(next, core);
    modes[id] = next;
    values = { ...values };
    modes = { ...modes };
    clearFieldError(id);
  }

  function syncMode(sec, key) {
    const id = fieldId(sec.id, key);
    const raw = values[id] || '';
    modes[id] = raw.trim() ? detectMode(raw) : modes[id] || 'css';
    modes = { ...modes };
  }

  function setValue(sec, key, event) {
    const id = fieldId(sec.id, key);
    values[id] = event.currentTarget.value;
    values = { ...values };
    clearFieldError(id);
  }

  function clearFieldError(id) {
    if (localErrors[id]) {
      const next = { ...localErrors };
      delete next[id];
      localErrors = next;
    }
    localBlocked = 0;
  }

  // ------------------------------------------------------------------
  // 校验：后端错误按字段定位；保存前做浅校验
  // ------------------------------------------------------------------
  function errorTargets(msg) {
    const s = String(msg || '');
    if (/bookSourceUrl/i.test(s)) return ['base.url'];
    if (/bookSourceName/i.test(s)) return ['base.name'];
    if (/ruleSearch/i.test(s) || /searchUrl/i.test(s)) return ['search.book_list'];
    if (/ruleToc/i.test(s) || /chapterList/i.test(s)) return ['toc.chapter_list'];
    if (/ruleContent/i.test(s)) return ['content.content'];
    return [];
  }

  function mapBackendErrors(v) {
    const out = {};
    if (!v || !Array.isArray(v.errors)) return out;
    v.errors.forEach((msg) => {
      const keys = errorTargets(msg);
      if (!keys.length) {
        (out._general || (out._general = [])).push(msg);
        return;
      }
      keys.forEach((key) => (out[key] || (out[key] = [])).push(msg));
    });
    return out;
  }

  function localCheck() {
    const errs = {};
    const url = String(base.book_source_url || '').trim();
    if (!url) {
      errs['base.url'] = ['书源 URL（bookSourceUrl）不能为空'];
    } else {
      let ok = false;
      try {
        const u = new URL(url);
        ok = u.protocol === 'http:' || u.protocol === 'https:';
      } catch {
        ok = false;
      }
      if (!ok) errs['base.url'] = ['书源 URL 必须是合法的 http(s) 地址'];
    }
    // 只有存在目录规则时，空 chapterList 才阻塞（纯搜索源可以没有 ruleToc）
    if (base.rule_toc && typeof base.rule_toc === 'object') {
      const v = values['toc.chapter_list'] || '';
      if (!v.trim()) errs['toc.chapter_list'] = ['目录规则 chapterList 不能为空'];
    }
    return errs;
  }

  function openErrorFields(errs) {
    const next = { ...openState };
    Object.keys(errs).forEach((key) => {
      const secId = key.split('.')[0];
      if (secId !== 'base' && sectionById(secId)) next[secId] = true;
    });
    openState = next;
    setTimeout(() => {
      const el = typeof document !== 'undefined' ? document.querySelector('.src-editor .field-err') : null;
      if (el && el.scrollIntoView) el.scrollIntoView({ block: 'center' });
    }, 80);
  }

  function doSave() {
    const errs = localCheck();
    localErrors = errs;
    localBlocked = Object.keys(errs).length;
    if (localBlocked) {
      openErrorFields(errs);
      return;
    }
    dispatch('save', { source: buildPayload() });
  }

  function doValidate() {
    dispatch('validate', { source: buildPayload() });
  }

  function buildPayload() {
    const out = clone(base);
    // 详情接口附加的派生字段不写回
    delete out.type;
    delete out.type_name;
    delete out.unsupported_type;

    out.book_source_name = String(out.book_source_name || '');
    out.book_source_url = String(out.book_source_url || '');
    out.book_source_group = String(out.book_source_group || '');
    out.book_source_type = Number(out.book_source_type) || 0;
    // 非 0/2 类型后端会强制禁用
    out.enabled = typeSupported ? !!out.enabled : false;
    out.search_url = String(out.search_url || '');
    out.book_source_comment = String(out.book_source_comment || '');

    RULE_META.forEach(({ ruleKey, secIds }) => {
      delete out[ruleKey];
      const obj = {};
      secIds.forEach((secId) => {
        const sec = sectionById(secId);
        const keys = [...knownKeys(sec), ...(extraKeys[secId] || [])];
        keys.forEach((key) => {
          const v = values[fieldId(secId, key)];
          if (v === undefined || v === null || String(v).trim() === '') return;
          obj[key] = String(v);
        });
      });
      if (Object.keys(obj).length) out[ruleKey] = obj;
    });
    return out;
  }

  // ------------------------------------------------------------------
  // 从 JSON 粘贴导入（整段规则覆盖表单）
  // ------------------------------------------------------------------
  function normalizeRule(rv, sec) {
    const map = {};
    sec.fields.forEach(([key]) => {
      map[key] = key;
      map[snakeToCamel(key)] = key;
    });
    const out = {};
    Object.keys(rv).forEach((k) => {
      out[map[k] || k] = rv[k];
    });
    return out;
  }

  function importJson() {
    pasteError = '';
    pasteNotice = '';
    let raw;
    try {
      raw = JSON.parse(pasteText);
    } catch (e) {
      pasteError = 'JSON 解析失败：' + e.message;
      return;
    }
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) {
      pasteError = '请粘贴一个 JSON 对象（完整书源或规则片段）';
      return;
    }

    const next = clone(base);
    const look = raw.rules && typeof raw.rules === 'object' && !Array.isArray(raw.rules) ? raw.rules : raw;
    let baseCount = 0;
    Object.entries(BASE_KEYS).forEach(([snake, camel]) => {
      if (snake in raw || camel in raw) {
        next[snake] = snake in raw ? raw[snake] : raw[camel];
        baseCount += 1;
      }
    });

    let ruleCount = 0;
    SECTIONS.forEach((sec) => {
      if (sec.imageOnly) return;
      const found = RULE_KEYS.find(([snake, camel]) => snake === sec.ruleKey && (snake in look || camel in look));
      if (!found) return;
      const rv = look[found[0]] !== undefined ? look[found[0]] : look[found[1]];
      if (!rv || typeof rv !== 'object' || Array.isArray(rv)) return;
      next[sec.ruleKey] = normalizeRule(rv, sec);
      ruleCount += 1;
    });

    // 停靠在 ruleContent 里的图片字段也归一到规则对象（normalizeRule 已保留未知键）
    if (next.rule_content && typeof next.rule_content === 'object') {
      Object.keys(next.rule_content).forEach((k) => {
        if (k === 'imageStyle') next.rule_content.image_style = next.rule_content[k];
        if (k === 'imageDecode') next.rule_content.image_decode = next.rule_content[k];
      });
      delete next.rule_content.imageStyle;
      delete next.rule_content.imageDecode;
    }

    if (!baseCount && !ruleCount) {
      pasteError = '没有识别到书源字段或规则段（支持 snake_case / camelCase，如 bookSourceUrl / ruleSearch）';
      return;
    }

    hydrate(next);
    localErrors = {};
    localBlocked = 0;
    pasteOpen = false;
    pasteText = '';
    pasteNotice = `已导入 ${ruleCount} 个规则段、${baseCount} 个基础字段`;
    setTimeout(() => (pasteNotice = ''), 3000);
  }

  function errText(list) {
    return Array.isArray(list) ? list.join('；') : String(list || '');
  }

  function panelErrCount(secId, errMap) {
    return Object.keys(errMap)
      .filter((k) => k.startsWith(secId + '.'))
      .reduce((n, k) => n + (Array.isArray(errMap[k]) ? errMap[k].length : 1), 0);
  }
</script>

<div class="src-editor">
  <div class="row ed-head">
    <h2 style="margin:0;flex:1;min-width:0">书源字段编辑</h2>
    <button class="ghost" on:click={() => dispatch('cancel')} disabled={saving}>关闭</button>
  </div>

  <div class="paste-box">
    <button class="ghost" on:click={() => (pasteOpen = !pasteOpen)}>
      {pasteOpen ? '收起 JSON 导入' : '从 JSON 粘贴导入'}
    </button>
    {#if pasteOpen}
      <textarea
        rows="5"
        bind:value={pasteText}
        placeholder={'粘贴完整书源 JSON，或只粘贴规则片段，例如：\n{"ruleSearch":{"bookList":"@css:.book-list","name":"@css:.name"}}'}
        aria-label="粘贴书源 JSON"
      ></textarea>
      <div class="row" style="margin-top:8px">
        <button on:click={importJson}>覆盖表单</button>
        <button class="ghost" on:click={() => { pasteText = ''; pasteError = ''; }}>清空</button>
      </div>
      <div class="muted" style="font-size:12px;line-height:1.5;margin-top:6px">
        支持 snake_case / camelCase；只会覆盖 JSON 里出现的规则段和基础字段，其余字段保持不变。
      </div>
      {#if pasteError}<div class="field-err">{pasteError}</div>{/if}
    {/if}
    {#if pasteNotice}<div class="ok-line" style="margin:6px 0 0">{pasteNotice}</div>{/if}
  </div>

  {#if saveError}
    <div class="warn-box"><div class="warn-title">保存未成功</div><div class="warn-item">{saveError}</div></div>
  {/if}
  {#if localBlocked}
    <div class="warn-box">
      <div class="warn-title">保存前检查未通过</div>
      <div class="warn-item">{localBlocked} 项必填有误，已展开并标在对应输入框下方。</div>
    </div>
  {/if}

  {#if validation}
    <div class="val-box" class:val-ok={validation.valid}>
      <div class="val-title">
        {validation.valid ? '校验通过' : `校验未通过：${(validation.errors || []).length} 个错误`}
      </div>
      {#if generalErrors.length}
        <ul class="val-list">
          {#each generalErrors as msg}<li>{msg}</li>{/each}
        </ul>
      {/if}
      {#if warnings.length}
        <div class="val-sub">警告（不阻塞保存）</div>
        <ul class="val-list">
          {#each warnings as msg}<li>{msg}</li>{/each}
        </ul>
      {/if}
      {#if !validation.valid && (validation.errors || []).length}
        <div class="val-sub">具体字段的错误已标在输入框下方。</div>
      {/if}
    </div>
  {/if}

  <!-- 基础信息 -->
  <section class="base-sec">
    <h3>基础信息</h3>
    <div class="grid cols2">
      <div class="bf">
        <div class="rf-head"><span class="rf-cn">名称</span><code class="rf-key">bookSourceName</code></div>
        <input bind:value={base.book_source_name} aria-label="书源名称" />
        {#if shownErrors['base.name']}<div class="field-err">{errText(shownErrors['base.name'])}</div>{/if}
      </div>
      <div class="bf">
        <div class="rf-head"><span class="rf-cn">分组</span><code class="rf-key">bookSourceGroup</code></div>
        <input bind:value={base.book_source_group} aria-label="书源分组" />
      </div>
    </div>

    <div class="bf" style="margin-top:10px">
      <div class="rf-head"><span class="rf-cn">URL（书源 ID）</span><code class="rf-key">bookSourceUrl</code></div>
      <input bind:value={base.book_source_url} aria-label="书源 URL" placeholder="https://example.com" />
      {#if shownErrors['base.url']}<div class="field-err">{errText(shownErrors['base.url'])}</div>{/if}
    </div>

    <div class="grid cols2" style="margin-top:10px">
      <div class="bf">
        <div class="rf-head"><span class="rf-cn">类型</span><code class="rf-key">bookSourceType</code></div>
        <select bind:value={base.book_source_type} aria-label="书源类型">
          <option value="0">0 · 文本</option>
          <option value="2">2 · 漫画</option>
          {#if !typeSupported}<option value={String(typeNum)} disabled>{typeNum} · 不支持</option>{/if}
        </select>
      </div>
      <label class="bf enable-line">
        <input type="checkbox" bind:checked={base.enabled} disabled={!typeSupported} />
        <span>启用（<code class="rf-key">enabled</code>）</span>
      </label>
    </div>
    {#if !typeSupported}
      <div class="warn-box" style="margin:8px 0 0">
        <div class="warn-title">当前类型 {typeNum} 不支持阅读</div>
        <div class="warn-item">仅支持 0 文本 / 2 漫画。保存时后端会强制禁用该书源；改为 0 或 2 后才能重新启用。</div>
      </div>
    {/if}

    <div class="bf" style="margin-top:10px">
      <div class="rf-head"><span class="rf-cn">搜索地址</span><code class="rf-key">searchUrl</code></div>
      <input bind:value={base.search_url} aria-label="搜索地址" placeholder="https://example.com/search?key=" />
    </div>

    <div class="bf" style="margin-top:10px">
      <div class="rf-head"><span class="rf-cn">备注</span><code class="rf-key">bookSourceComment</code></div>
      <textarea rows="2" bind:value={base.book_source_comment} aria-label="书源备注"></textarea>
    </div>
  </section>

  <!-- 五段规则 -->
  {#each sections as sec (sec.id)}
    <details class="rule-panel" bind:open={openState[sec.id]}>
      <summary class="panel-head">
        <span class="panel-title">{sec.title}</span>
        <code class="panel-en">{sec.en}</code>
        {#if panelErrCount(sec.id, shownErrors) > 0}
          <span class="panel-err">有 {panelErrCount(sec.id, shownErrors)} 项错误</span>
        {/if}
      </summary>
      <div class="panel-body">
        {#each sec.rows as row (row.key)}
          {@const id = fieldId(sec.id, row.key)}
          <div class="rf" class:rf-bad={!!shownErrors[id]}>
            <div class="rf-head">
              <span class="rf-cn">{row.label || '自定义字段'}</span>
              <code class="rf-key">{row.key}</code>
            </div>
            <div class="rf-body">
              <select
                class="rf-mode"
                title="规则模式：CSS / JsonPath / XPath / Regex / JS"
                value={modeFor(sec, row.key, modes)}
                on:change={(e) => changeMode(sec, row.key, e)}
                aria-label={`${row.label || row.key} 规则模式`}
              >
                {#each MODES as m}<option value={m.id}>{m.label}</option>{/each}
              </select>
              <textarea
                class="rf-val"
                data-field={id}
                rows={row.key === 'content' ? 4 : 2}
                spellcheck="false"
                value={values[id] ?? ''}
                on:input={(e) => setValue(sec, row.key, e)}
                on:blur={() => syncMode(sec, row.key)}
                aria-label={`${row.label || row.key} 规则值`}
                placeholder={row.key === 'content' ? '正文所在的 CSS / XPath / JSON 规则' : '规则值'}
              ></textarea>
            </div>
            {#if shownErrors[id]}
              <div class="field-err">{errText(shownErrors[id])}</div>
            {/if}
          </div>
        {/each}
      </div>
    </details>
  {/each}

  <div class="editor-actions">
    <button class="ghost" on:click={() => dispatch('cancel')} disabled={saving}>取消</button>
    <div class="spacer"></div>
    <button class="ghost" on:click={doValidate} disabled={saving || validating}>
      {validating ? '校验中…' : '校验'}
    </button>
    <button on:click={doSave} disabled={saving || validating}>{saving ? '保存中…' : '保存'}</button>
  </div>
</div>

<style>
  .src-editor {
    width: 100%;
    min-width: 0;
  }
  .ed-head {
    position: sticky;
    top: -20px;
    z-index: 3;
    background: #fff;
    padding: 4px 0 10px;
    border-bottom: 1px solid var(--border);
    margin-bottom: 10px;
  }
  .rf-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 8px;
    margin-bottom: 4px;
  }
  .rf-cn { font-size: 13px; font-weight: 600; }
  .rf-key {
    font-size: 11px;
    color: var(--muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
    word-break: break-all;
    text-align: right;
  }
  .bf { min-width: 0; }
  .enable-line {
    display: flex;
    align-items: center;
    gap: 8px;
    padding-top: 18px;
    font-size: 13px;
  }
  .enable-line input[type='checkbox'] {
    width: auto;
    min-width: 18px;
    height: 18px;
  }
  .base-sec {
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 12px;
    margin-bottom: 12px;
  }
  .base-sec h3 { font-size: 14px; }
  .paste-box { margin-bottom: 10px; }
  .paste-box textarea { margin-top: 8px; }
  .rule-panel {
    border: 1px solid var(--border);
    border-radius: 10px;
    margin-bottom: 10px;
    overflow: hidden;
  }
  .panel-head {
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    padding: 10px 12px;
    cursor: pointer;
    background: #fafafa;
    list-style-position: inside;
  }
  .panel-title { font-size: 14px; font-weight: 600; }
  .panel-en {
    font-size: 11px;
    color: var(--muted);
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .panel-err {
    margin-left: auto;
    font-size: 12px;
    color: #b3392f;
    border: 1px solid #e3b4ae;
    border-radius: 999px;
    padding: 1px 8px;
  }
  .panel-body {
    padding: 10px 12px 12px;
    border-top: 1px solid var(--border);
  }
  .rf { margin-bottom: 12px; min-width: 0; }
  .rf:last-child { margin-bottom: 0; }
  .rf-body {
    display: flex;
    gap: 8px;
    align-items: flex-start;
    flex-wrap: wrap;
    min-width: 0;
  }
  .rf-mode {
    flex: 0 0 112px;
    width: 112px;
    min-width: 0;
    padding: 8px 6px;
    font-size: 12px;
  }
  .rf-val {
    flex: 1 1 180px;
    width: 100%;
    min-width: 0;
    font-family: ui-monospace, SFMono-Regular, Menlo, monospace;
  }
  .rf-bad .rf-mode,
  .rf-bad .rf-val { border-color: #d9534f; }
  .field-err {
    color: #b3392f;
    font-size: 12px;
    line-height: 1.4;
    margin-top: 4px;
    word-break: break-word;
  }
  .val-box {
    border: 1px solid #e3b4ae;
    background: #fdf3f2;
    border-radius: 10px;
    padding: 10px 12px;
    margin-bottom: 10px;
    font-size: 12px;
  }
  .val-box.val-ok {
    border-color: #b7d3bf;
    background: #f2f9f4;
  }
  .val-title { font-weight: 600; font-size: 13px; margin-bottom: 4px; }
  .val-sub { color: var(--muted); margin-top: 6px; }
  .val-list { margin: 4px 0 0; padding-left: 18px; line-height: 1.5; word-break: break-word; }
  .editor-actions {
    position: sticky;
    bottom: 0;
    z-index: 3;
    display: flex;
    align-items: center;
    gap: 8px;
    flex-wrap: wrap;
    background: #fff;
    border-top: 1px solid var(--border);
    padding: 10px 0;
  }
  /* 吸底操作栏下方补一层白底，避免滚动内容从 modal 内边距里透出来 */
  .editor-actions::after {
    content: '';
    position: absolute;
    left: 0;
    right: 0;
    top: 100%;
    height: 24px;
    background: #fff;
  }
  :global(html[data-vp='sm']) .ed-head { top: -14px; }
  :global(html[data-vp='sm']) .rf-mode {
    flex: 0 0 96px;
    width: 96px;
  }
  :global(html[data-vp='sm']) .rf-val { flex: 1 1 160px; }
  :global(html[data-device='eink']) .base-sec,
  :global(html[data-device='eink']) .rule-panel,
  :global(html[data-device='eink']) .panel-head,
  :global(html[data-device='eink']) .panel-body,
  :global(html[data-device='eink']) .val-box {
    border-color: #000;
    background: #fff;
  }
  :global(html[data-device='eink']) .panel-en,
  :global(html[data-device='eink']) .rf-key { color: #333; }
  :global(html[data-device='eink']) .panel-err {
    color: #000;
    border-color: #000;
  }
  :global(html[data-device='eink']) .field-err { color: #000; font-weight: 700; }
</style>
