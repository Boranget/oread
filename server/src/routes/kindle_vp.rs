//! /k 的 vp 部分（move-only 从 kindle.rs 拆出）。

use super::*;

/// 请求级视口（CSS px）。几何全部由它推导，不再写死 1000/1268；
/// 优先级：请求参数 `vw`/`vh` > 环境变量默认（见 `Config.kindle_vw/vh`）。
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Vp {
    pub(super) w: usize,
    pub(super) h: usize,
    /// 请求是否显式带了 `vw`。显式时 viewport 锁 `width={w}`（真机 1:1 不缩放）；
    /// 缺参时用 `width=device-width`，让补参脚本能量出真实 CSS 宽度。
    pub(super) fixed: bool,
}

/// 视口合法范围（CSS px）。下限 240 覆盖手机（常见 360–430），上限 2400 防异常值撑爆版面；
/// 服务端校验与 `K_VP_JS` 共用这两个值（脚本用 `$MIN`/`$MAX` 占位注入），不两处手写。
pub(super) const VP_MIN: i64 = 240;
pub(super) const VP_MAX: i64 = 2400;

/// 统一缩放下限（百分比）：390 宽手机 → 50%（正文 32 → 16px）。低于此值不再缩小，
/// 否则次要文字会掉到不可读；屏幕更窄时靠单列布局容纳，而不是继续缩字。
pub(super) const SCALE_MIN_PCT: usize = 50;
/// 文本最小字号（px）
pub(super) const TEXT_MIN: usize = 13;
/// 可点区域最小高度（px）：触控热区不随缩放变小
pub(super) const TAP_MIN: usize = 44;
/// 左右留白下限（px）
pub(super) const PAD_MIN: usize = 12;
/// 窄屏阈值（视口宽 CSS px）：只按宽度判定，手机、窄窗口、平板分屏走同一套
pub(super) const NARROW_W: usize = 744;

impl Vp {
    /// 缺参/越界时回落到默认视口，保证任何尺寸都不会算出负高度。
    pub(super) fn resolve(vw: Option<i64>, vh: Option<i64>, d: (usize, usize)) -> Self {
        let ok = |v: Option<i64>| {
            v.filter(|n| (VP_MIN..=VP_MAX).contains(n))
                .map(|n| n as usize)
        };
        Self {
            w: ok(vw).unwrap_or(d.0),
            h: ok(vh).unwrap_or(d.1),
            fixed: ok(vw).is_some(),
        }
    }
    /// 统一缩放百分比：以参考视口 1072 为 100%，下限 50%（390 宽手机 → 50%）。
    /// 所有字号、行高、留白都从它派生，**不再有第二处比例**。
    pub(super) fn pct(self) -> usize {
        (self.w * 100 / K_W).clamp(SCALE_MIN_PCT, 100)
    }
    /// 按比例换算长度（不设下限）：宽度、缩略图尺寸这类纯几何值用它。
    pub(super) fn len(self, base: usize) -> usize {
        base * self.pct() / 100
    }
    /// 文本尺寸：按比例缩放，下限 `TEXT_MIN`（13px），次要信息在手机上仍可读。
    pub(super) fn px(self, base: usize) -> usize {
        self.len(base).max(TEXT_MIN)
    }
    /// 可点区域 / 成块高度：按比例缩放，下限 `TAP_MIN`（44px），触控热区不随缩放变小。
    pub(super) fn tap(self, base: usize) -> usize {
        self.len(base).max(TAP_MIN)
    }
    /// 左右留白：按比例缩放，下限 `PAD_MIN`（12px）。手机上 36px 会吃掉 18% 屏宽。
    pub(super) fn pad(self) -> usize {
        self.len(SIDE_PAD).max(PAD_MIN)
    }
    /// 版心宽 = 视口宽 − 左右留白 ×2（1072 → 1000，390 → 354）
    pub(super) fn page_w(self) -> usize {
        self.w.saturating_sub(self.pad() * 2)
    }
    /// 漫画图宽 = 版心宽 − 左右留白 ×2（1072 → 928，即切片版心宽 dw）
    pub(super) fn content_w(self) -> usize {
        self.w.saturating_sub(self.pad() * 4)
    }
    /// 文档高 = 请求带回来的运行时实测可见高（`K_VP_JS` 现算，见 `layout_h` 上方说明）。
    /// 不再做任何固定 px 收缩/夹取：顶栏、底栏、正文切片都从这一个值派生。
    pub(super) fn layout_h(self) -> usize {
        self.h
    }
    /// 阅读页底栏总高（含底部安全区）：随运行时可用高派生，约 5.2%（1231 → 64），
    /// 不写死像素；顶栏、底栏、正文区都从 `layout_h()` 这一个值派生。
    pub(super) fn bar_h(self) -> usize {
        (self.layout_h() * 13 / 250).max(48)
    }
    /// 底栏内容高（去掉按比例缩放后的安全区），保证格高 + 安全区不超过底栏总高
    pub(super) fn bar_content_h(self) -> usize {
        self.bar_h().saturating_sub(self.len(SAFE_BOTTOM))
    }
    /// 阅读页正文容器高 = 文档高 − 底栏
    pub(super) fn body_h(self) -> usize {
        self.layout_h().saturating_sub(self.bar_h())
    }
    /// 列表页顶栏总高（73 = 72 内容 + 1px 下边线；窄屏压到 44 触控下限）
    pub(super) fn nav_bar_h(self) -> usize {
        self.tap(NAV_H + 1)
    }
    /// 带顶栏页面的正文区高 = 文档高 − 顶栏（1268 → 1195，844 → 800）
    pub(super) fn nav_h(self) -> usize {
        self.layout_h().saturating_sub(self.nav_bar_h())
    }
    /// 列表页翻页条高（64 → 44 触控下限）
    pub(super) fn pager_h(self) -> usize {
        self.tap(BAR_H)
    }
    /// 目录行高（72 → 44 触控下限）
    pub(super) fn toc_row_h(self) -> usize {
        self.tap(TOC_ROW_H)
    }
    /// 阅读页顶栏高（56 → 44 触控下限）
    pub(super) fn read_nav_h(self) -> usize {
        self.tap(READ_NAV_H)
    }
    /// 阅读页正文顶部留白（36 → 18）
    pub(super) fn read_pad_top(self) -> usize {
        self.px(READ_PAD_TOP)
    }
    /// 正文底部页码行高（56 → 28）
    pub(super) fn read_pageno_h(self) -> usize {
        self.px(READ_PAGENO_H)
    }
    /// 小说正文可用高 = 正文容器 − 顶栏 − 顶部留白 − 页码行（1268 → 1055）
    pub(super) fn text_h(self) -> usize {
        self.body_h()
            .saturating_sub(self.read_nav_h() + self.read_pad_top() + self.read_pageno_h())
    }
    /// 指定档位的实际字号/行高（参考表 × 统一比例）：分页与渲染必须用同一份。
    pub(super) fn fs(self, fs: usize) -> FsSpec {
        let spec = &FS_SPECS[fs - 1];
        FsSpec {
            size: self.px(spec.size),
            line: self.px(spec.line),
            label: spec.label,
        }
    }
    /// 漫画整图/单片可用高 = 文档高 − 底栏 − 8（同一运行时高派生）
    pub(super) fn content_h(self) -> usize {
        self.layout_h().saturating_sub(self.bar_h() + 8)
    }
    /// 窄屏（视口宽 < 744）：目录单列、表单堆叠、顶栏动作压缩。
    /// 只按宽度判定，与设备类型无关：手机、窄窗口、平板分屏走同一套。
    pub(super) fn narrow(self) -> bool {
        self.w < NARROW_W
    }
    /// 目录列数：窄屏单列，宽屏两列（每列最多 `K_TOC_PAGE / K_TOC_COLS` 行）。
    pub(super) fn toc_cols(self) -> usize {
        if self.narrow() {
            1
        } else {
            K_TOC_COLS
        }
    }
    /// 左右翻页热区宽 = 视口宽 / 3（1072 → 357，CSS 用 33% 自适应）
    #[allow(dead_code)] // 仅单测引用：渲染改用 CSS 百分比，不再需要服务端算像素
    pub(super) fn hot_w(self) -> usize {
        self.w / 3
    }
    /// 书架每页本数与行高：行高按统一比例缩放（112 → 56），把可用高（`nav_h − 翻页条`）
    /// 均分给整行；参考视口仍是 10 本/页，窄屏行变矮后多排几本（上限 `rows_cap`），
    /// 视口极矮时减少本数，列表永不超过 stage。
    pub(super) fn shelf_layout(self) -> (usize, usize) {
        let avail = self.nav_h().saturating_sub(self.pager_h());
        let row_h = (avail / K_SHELF_PAGE).clamp(self.tap(56), self.tap(K_SHELF_ROW_H));
        let per_page = (avail / row_h).clamp(1, self.rows_cap(K_SHELF_PAGE));
        (per_page, row_h)
    }
    /// 行数上限：参考上限 × (100 / pct)。窄屏行变矮后仍能填满屏幕（1072 → 原值）。
    pub(super) fn rows_cap(self, base: usize) -> usize {
        (base * 100 / self.pct()).max(base)
    }
}

impl Default for Vp {
    fn default() -> Self {
        Self {
            w: K_W,
            h: K_H,
            fixed: true,
        }
    }
}

/// 阅读页每页行数预算 = 可用高 ÷ 行高（字号四档 28/32/38/20 对应 46/52/62/36）。
/// 动态算出，不再有 24/21/17/25 这类硬编码常量。
pub(super) fn line_budget(vp: Vp, line: usize) -> usize {
    (vp.text_h() / line.max(1)).max(1)
}
/// 段间距（行）：WeRead 实测段距 0.6 行（默认档 27px）
/// 实际排版用 `para_gap_px`（整数 px）；本常量保留为换算基准，仅单测引用。
#[allow(dead_code)]
pub(super) const PARA_GAP_LINES: f64 = 0.6;
/// 段间距像素（向下取整，保证渲染高度不超过行预算）
pub(super) const fn para_gap_px(line: usize) -> usize {
    (line * 6) / 10
}
/// 目录每页章节数 = 每列行数 × 列数（窄屏单列）；每列行数按可用高实算（行高随缩放）。
pub(super) fn toc_page_size(vp: Vp) -> usize {
    let rows = (vp.nav_h().saturating_sub(vp.pager_h()) / vp.toc_row_h())
        .clamp(1, vp.rows_cap(K_TOC_PAGE / K_TOC_COLS));
    rows * vp.toc_cols()
}
/// 搜索结果每页条数 = （正文区 − 表单 − 翻页条）/ 行高；表单高度窄屏是两行输入。
/// `extra_rows` 是状态提示/回退说明/升级按钮额外占用的行数。
pub(super) fn search_page_size_extra(vp: Vp, extra_rows: usize) -> usize {
    let form_h = if vp.narrow() {
        vp.tap(56) * 2 + vp.len(12) + vp.len(32)
    } else {
        vp.tap(88)
    };
    let row_h = vp.tap(112);
    let base = ((vp.nav_h().saturating_sub(vp.pager_h() + form_h)) / row_h)
        .clamp(1, vp.rows_cap(K_SEARCH_PAGE));
    base.saturating_sub(extra_rows).max(1)
}
/// 目录每页章节数上限：两列各 15 行 × 72px = 1080（实际行数按可用高实算）
pub(super) const K_TOC_PAGE: usize = 30;
/// 目录每列行数
pub(super) const K_TOC_COLS: usize = 2;
/// 书架每页本数上限：10 行 × 112px = 1120（实际行高按可用高等分，见 `Vp::shelf_layout`）
pub(super) const K_SHELF_PAGE: usize = 10;
/// 搜索结果每页条数上限：9 行 × 112px = 1008（实际条数按可用高实算）
pub(super) const K_SEARCH_PAGE: usize = 9;
/// /k 渲染前对正文纯文本的硬截断（防御超大章节把分页临时索引撑爆内存）
pub(super) const K_MAX_TEXT_CHARS: usize = 800_000;
/// 超长章节分块：每块最多 20 万字符，按块分页，避免一次性按 80 万字构建全量页表
pub(super) const K_CHUNK_CHARS: usize = 200_000;
/// 字号 Cookie（与 `?fs=` 二选一，显式参数优先）
pub(super) const K_FS_COOKIE: &str = "k_fs";

/// 运行时视口脚本（ES5，注入 `<head>`，首帧前生效；不落 Cookie、不写 sessionStorage）。
///
/// 每次进页面现算视口，再把 `vw=<宽>&vh=<高>` 带回 URL（缺哪个补哪个，最多 replace 一次）：
///
///   高：1. `outerHeight − 1`（真机 1232 → 1231，最接近真实可用高）与 `innerHeight`
///       取较小者（手机 innerHeight 才是可视区，Kindle 上 innerHeight=1268 更大、不生效）；
///       2. `screen.availHeight` 做上限保护；
///       3. 都取不到才退 `innerHeight`（1268 只作机型识别与兜底）。
///   宽：缺 `vw` 时取 `documentElement.clientWidth`（首屏 meta 是 `width=device-width`，
///       即设备真实 CSS 宽），取不到再退 `innerWidth`；KPW3 量出 1072，与旧行为一致。
///
/// `vw` 已带合法值（`VP_MIN`–`VP_MAX`）时原样保留，否则用实测宽度补齐；`vh` 永远以本次实测为准
/// （只作链接传输，不当用户偏好）。`load` 后把同一组参数补到页内所有链接，翻页不再二次重载。
/// 同机型反复进入结果一致。返回注入边界值后的脚本。
pub(super) fn k_vp_js() -> String {
    K_VP_JS
        .replace("$MIN", &VP_MIN.to_string())
        .replace("$MAX", &VP_MAX.to_string())
}

const K_VP_JS: &str = "(function(){try{\
var d=document.documentElement,ih=window.innerHeight||0,iw=window.innerWidth||0,sh=window.screen||{};\
function mn(a,b){if(!(b>0))return a;if(!(a>0))return b;return a<b?a:b}\
var H=(window.outerHeight||0)-1,AH=(sh.availHeight||0);\
if(H>200&&ih>200)H=mn(H,ih);\
if(!(H>200))H=AH;\
if(AH>200)H=mn(H,AH);\
if(!(H>200))H=ih;\
if(!(H>200))H=(d&&d.clientHeight>0)?d.clientHeight:ih;\
var W=(d&&d.clientWidth>0)?d.clientWidth:iw;\
var s=location.search.replace(/^\\?/,''),p=s?s.split('&'):[],o=[],cv=null,ch=null;\
for(var i=0;i<p.length;i++){var k=p[i].split('=')[0];\
if(k=='vw')cv=p[i].substring(3);else if(k=='vh')ch=p[i].substring(3);else if(p[i])o.push(p[i]);}\
if(!(parseInt(cv,10)>=$MIN&&parseInt(cv,10)<=$MAX))cv=null;\
if(!(W>=$MIN&&W<=$MAX))W=0;\
window.__kvh=H;\
var vw=cv||(W?''+W:null),vh=''+H;\
if((vw&&vw!=cv)||ch!=vh){if(vw)o.push('vw='+vw);o.push('vh='+vh);\
location.replace(location.pathname+'?'+o.join('&'));return}\
function fix(){if(!document.links)return;try{\
for(var j=0;j<document.links.length;j++){var a=document.links[j],h=a.getAttribute('href');\
if(!h||h.charAt(0)=='#'||h.indexOf('javascript:')==0)continue;\
var hw=h.indexOf('vw=')>-1,hv=h.indexOf('vh=')>-1;\
if(hw&&hv)continue;\
var add=[];if(vw&&!hw)add.push('vw='+vw);if(!hv)add.push('vh='+vh);\
a.setAttribute('href',h+(h.indexOf('?')>-1?'&':'?')+add.join('&'))}}catch(e){}}\
if(document.addEventListener){document.addEventListener('load',fix,false)}setTimeout(fix,0)\
}catch(E){try{window.__kerr&&window.__kerr((E&&E.message)||E,0,'vp')}catch(x){}}})()";

/// 全局错误兜底（ES5，注入 `<head>`）。错误统一走 `window.__kerr`，用 `<img>` 打点上报
/// （`stage=gerr`），不依赖 `load` 与 XHR。**不再拦截 touchmove、也不再上报 touch 事件**
/// （真机实测 touch 不触发；且已明确不做任何「骗浏览器」的补偿）。
pub(super) const K_GUARD_JS: &str = "(function(){\
function e(m,l,st){try{var i=new Image();\
i.src='/api/k/probe?stage='+st+'&err='+encodeURIComponent((''+(m||'-')).substring(0,60))+\
'&ln='+(l||0)+'&vw_param='+encodeURIComponent((''+(location.search||'')).substring(0,80))}catch(x){}}\
function ke(m,s,l){e(m,l,'gerr');return false}\
window.__kerr=e;window.onerror=ke})()";

/// 自报探针（ES5，零依赖，不重载页面）：**只保留 `<img>` 单通道**（v0.3.19 的教训：
/// img 请求 204 触发 `onerror` 后又走 XHR = 每个 stage 打两条，统计翻倍）。
/// 打点时机：`sync`（`setTimeout(...,0)` 延后到首次布局完成，避免 `inner_h=0`）、
/// `load`、`scroll`（200ms 节流 + 800ms settle）、`click`。整段包在 `try/catch` 里，
/// 异常走 `window.__kerr`（`stage=probe`）。
/// v0.3.22 瘦身：删掉标定/vmh 字段（`calib`/`meas_y`/`vv_off`/`vw_meta_height`/`meta_h`/
/// `visible_h`），改为上报运行时参与计算的原值与结果：`outer_h`/`inner_h`/`screen_h`/
/// `avail_h` + `rt_h`（`window.__kvh` 现算高）+ `lines`（`#k-stage` 的 `data-lb` 每页行数）。
/// 溢出证据看 `doc_h` / `documentElement.scrollHeight` / `outer_minus_doc` /
/// `outer_minus_scroll`；`body_scroll_h` 在该引擎里是假象，只作对拍。
/// `click` 上报 `pageX/pageY`（真机 touch 不触发，click 可用）与内层滚动容器 `scrollTop`。
pub(super) const K_PROBE_JS: &str = "(function(){try{\
function p(n){var m=new RegExp('[?&]'+n+'=([^&]*)').exec(location.search);return m?m[1]:''}\
function v(x){return typeof x=='number'?x:'-'}\
function bar(){return document.getElementById('k-bar')}\
function btn(){var b=bar();if(!b)return -1;try{return Math.round(b.getBoundingClientRect().top)}catch(e){\
return typeof b.offsetTop=='number'?b.offsetTop:-1}}\
function bt(){var n=btn();return n<0?'-':n}\
function bb(){var b=bar();if(!b)return '-';try{return Math.round(b.getBoundingClientRect().bottom)}catch(e){return b.offsetTop+b.offsetHeight}}\
function bre(){var b=document.body;if(!b)return '-';try{return Math.round(b.getBoundingClientRect().bottom)}catch(e){return '-'}}\
function lb(){var b=document.body;if(!b||!b.children)return '-';var m=-1;try{for(var i=0;i<b.children.length;i++){\
var r=b.children[i].getBoundingClientRect(),x=r.top+(r.height||(r.bottom-r.top));if(x>m)m=x}}catch(e){return '-'}return m>=0?Math.round(m):'-'}\
function stb(){try{return (document.body&&document.body.scrollTop)||0}catch(e){return 0}}\
function std(){try{var d=document.documentElement;return (d&&d.scrollTop)||0}catch(e){return 0}}\
function sy(){try{return Math.round(window.scrollY||window.pageYOffset||0)}catch(e){return 0}}\
var max_seen=0,base_bt=-1;\
function py(){var y=stb();if(!y)y=std();if(!y)y=sy();\
y=Math.round(y);if(y>max_seen)max_seen=y;return y}\
function btd(){if(base_bt<0)return '-';var n=btn();if(n<0)return '-';var d=n-base_bt;\
if(d<0)d=-d;if(d>max_seen)max_seen=d;return n-base_bt}\
function lbn(){try{var s=document.getElementById('k-stage');\
var n=s?parseInt(s.getAttribute('data-lb'),10):NaN;return isNaN(n)?'-':n}catch(e){return '-'}}\
function ist(){try{var s=document.getElementById('k-scroll');\
return s?Math.round(s.scrollTop||0):'-'}catch(e){return '-'}}\
function qs(ev){var d=document.documentElement,dpr=window.devicePixelRatio||1,sn=window.screen||{},b=document.body,\
st=document.getElementById('k-stage'),sb=stb(),sd=std(),dlt=btd(),\
dh=parseFloat(d.style.height);if(!(dh>0))dh=d.clientHeight;var oh=window.outerHeight||0;\
return 'vw_param='+p('vw')+'&vh_param='+p('vh')+'&inner_w='+innerWidth+'&inner_h='+innerHeight+\
'&client_w='+d.clientWidth+'&client_h='+d.clientHeight+'&scroll_w='+d.scrollWidth+'&scroll_h='+d.scrollHeight+\
'&dpr='+dpr+'&fs='+p('fs')+'&path='+encodeURIComponent(location.pathname)+\
'&screen_w='+v(sn.width)+'&screen_h='+v(sn.height)+'&avail_h='+v(sn.availHeight)+\
'&outer_w='+v(window.outerWidth)+'&outer_h='+v(window.outerHeight)+\
'&doc_h='+Math.round(dh)+'&doc_minus_outer='+Math.round(dh-oh)+'&doc_minus_client='+Math.round(dh-d.clientHeight)+\
'&outer_minus_doc='+Math.round(oh-dh)+'&rt_h='+v(window.__kvh)+'&lines='+lbn()+\
'&body_h='+v(b&&b.clientHeight)+'&stage_h='+v(st&&st.clientHeight)+\
'&bar_top='+bt()+'&bar_bottom='+bb()+'&bar_rect_top='+bt()+'&bar_top_delta='+dlt+\
'&page_y='+py()+'&doc_top='+sd+'&scroll_top_body='+sb+'&scroll_top_doc='+sd+'&scroll_y='+sy()+\
'&max_seen='+max_seen+\
'&body_scroll_h='+v(b&&b.scrollHeight)+\
'&body_rect_bottom='+bre()+'&lowest_el_bottom='+lb()+'&scroll_evt='+(ev?1:0)+\
'&outer_minus_scroll='+Math.round(oh-d.scrollHeight)+'&inner_scroll_top='+ist()}\
function s(stage,src){try{var u='/api/k/probe?'+qs(src=='scroll'?1:0)+\
'&stage='+stage+'&move_src='+(src||'-');var i=new Image();i.src=u}catch(e){\
try{window.__kerr&&window.__kerr((e&&e.message)||e,0,'probe')}catch(x){}}}\
var last=0,stt=0;\
function mv(src){var t=new Date().getTime();if(t-last<200)return;last=t;s('move',src)}\
function ld(){if(base_bt<0)base_bt=btn();s('load','-')}\
function rs(){mv('scroll');if(stt)clearTimeout(stt);\
stt=setTimeout(function(){s('move','settle')},800)}\
function cl(ev){try{var d=document.documentElement,x=ev.pageX,y=ev.pageY;\
if(typeof x!='number')x=ev.clientX;if(typeof y!='number')y=ev.clientY;\
var i=new Image();i.src='/api/k/probe?stage=click&click_x='+Math.round(x)+'&click_y='+Math.round(y)+\
'&scroll_h='+d.scrollHeight+'&outer_minus_scroll='+Math.round((window.outerHeight||0)-d.scrollHeight)+\
'&inner_scroll_top='+ist()+'&vw_param='+encodeURIComponent((''+(location.search||'')).substring(0,60))}catch(e){}}\
setTimeout(function(){s('sync','-')},0);\
if(window.addEventListener){window.addEventListener('load',ld,false);\
window.addEventListener('scroll',rs,false);document.addEventListener('click',cl,false)}\
else if(window.attachEvent){window.attachEvent('onload',ld);window.attachEvent('onscroll',rs)}\
}catch(E){try{window.__kerr&&window.__kerr((E&&E.message)||E,0,'probe')}catch(x){}}})()";

/// 四档字号。每页行数由 `line_budget(vp, line)` 按视口高动态算出。
pub(super) struct FsSpec {
    pub(super) size: usize,
    pub(super) line: usize,
    pub(super) label: &'static str,
}

pub(super) const FS_SPECS: [FsSpec; 4] = [
    // 927/28 ≈ 33 字/行；行数 = 正文可用高 ÷ 46。
    FsSpec {
        size: 28,
        line: 46,
        label: "小",
    },
    // 字号口径：真机实测 32px/52px，保留 32px。此处显式写死 32，且全局
    // `-webkit-text-size-adjust:none` 已关闭老引擎的字号自动放大——实测 32/52，勿低于 30。
    FsSpec {
        size: 32,
        line: 52,
        label: "中",
    },
    FsSpec {
        size: 38,
        line: 62,
        label: "大",
    },
    FsSpec {
        size: 20,
        line: 36,
        label: "超小",
    },
];
