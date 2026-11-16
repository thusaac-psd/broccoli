// TeX support for `Markdown`, loaded only for text that holds math: KaTeX
// and its fonts and styles are most of the Markdown renderer's size.
import 'katex/dist/katex.min.css';

import rehypeKatex from 'rehype-katex';
import remarkMath from 'remark-math';

export const remarkPlugins = [remarkMath];
export const rehypePlugins = [rehypeKatex];
