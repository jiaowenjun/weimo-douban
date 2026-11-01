import { useCallback } from 'react';
import type { RefCallback } from 'react';
import type { RefSelectProps } from 'antd';

/**
 * macOS WebKit 默认给文本框开自动拼写订正与自动大写，豆瓣 ID / 主页
 * 链接是纯字母数字串，被「订正」成英文单词反而干扰输入。antd
 * AutoComplete 上的属性会被 rc-select 白名单过滤、到不了内层 input，
 * 故以回调 ref 在真实 input 上直接关掉三件套（回调随挂载触发，弹窗
 * destroyOnClose 重挂后也会重新补上）。
 */
export function useNoSpellCheck(): RefCallback<RefSelectProps> {
  return useCallback((node: RefSelectProps | null) => {
    const input = node?.nativeElement?.querySelector('input');
    if (input) {
      input.setAttribute('autocorrect', 'off');
      input.setAttribute('autocapitalize', 'none');
      input.setAttribute('spellcheck', 'false');
    }
  }, []);
}
