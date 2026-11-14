import 'katex/dist/katex.min.css';

import { cn } from '@broccoli/web-sdk/utils';
import type { ComponentPropsWithoutRef } from 'react';
import ReactMarkdown from 'react-markdown';
import rehypeKatex from 'rehype-katex';
import remarkGfm from 'remark-gfm';
import remarkMath from 'remark-math';

import { AuthImage } from '@/components/AuthImage';
import { AuthLink } from '@/components/AuthLink';

interface MarkdownProps {
  children: string;
  className?: string;
  /** Rewrites link and image URLs before they are fetched or followed. */
  resolveUrl?: (url: string | undefined) => string | undefined;
}

function markdownCellAlignClass(
  align: ComponentPropsWithoutRef<'td'>['align'],
) {
  switch (align) {
    case 'center':
      return 'text-center';
    case 'right':
      return 'text-right';
    default:
      return 'text-left';
  }
}

export function Markdown({ children, className, resolveUrl }: MarkdownProps) {
  const resolve = resolveUrl ?? ((url: string | undefined) => url);
  return (
    <div className={className}>
      <ReactMarkdown
        children={children}
        remarkPlugins={[remarkGfm, remarkMath]}
        rehypePlugins={[rehypeKatex]}
        components={{
          h1: ({ children }: ComponentPropsWithoutRef<'h1'>) => (
            <h1 className="text-2xl font-bold mt-6 mb-2">{children}</h1>
          ),
          h2: ({ children }: ComponentPropsWithoutRef<'h2'>) => (
            <h2 className="text-xl font-semibold mt-5 mb-2">{children}</h2>
          ),
          h3: ({ children }: ComponentPropsWithoutRef<'h3'>) => (
            <h3 className="text-lg font-semibold mt-4 mb-1.5">{children}</h3>
          ),
          p: ({ children }: ComponentPropsWithoutRef<'p'>) => (
            <p className="text-sm leading-relaxed mb-3 text-foreground/90">
              {children}
            </p>
          ),
          ul: ({ children }: ComponentPropsWithoutRef<'ul'>) => (
            <ul className="text-sm list-disc pl-6 mb-3 space-y-1 text-foreground/90">
              {children}
            </ul>
          ),
          ol: ({ children }: ComponentPropsWithoutRef<'ol'>) => (
            <ol className="text-sm list-decimal pl-6 mb-3 space-y-1 text-foreground/90">
              {children}
            </ol>
          ),
          li: ({ children }: ComponentPropsWithoutRef<'li'>) => (
            <li className="leading-relaxed">{children}</li>
          ),
          blockquote: ({
            children,
          }: ComponentPropsWithoutRef<'blockquote'>) => (
            <blockquote className="border-l-[3px] border-foreground/20 pl-4 my-3 text-sm text-foreground/70 italic">
              {children}
            </blockquote>
          ),
          hr: () => <hr className="my-6 border-border" />,
          a: ({ href, children }: ComponentPropsWithoutRef<'a'>) => (
            <AuthLink
              href={resolve(href)}
              className="text-primary underline underline-offset-2 decoration-primary/40 hover:decoration-primary/80 transition-colors"
            >
              {children}
            </AuthLink>
          ),
          strong: ({ children }: ComponentPropsWithoutRef<'strong'>) => (
            <strong className="font-semibold text-foreground">
              {children}
            </strong>
          ),
          em: ({ children }: ComponentPropsWithoutRef<'em'>) => (
            <em className="italic">{children}</em>
          ),
          pre: ({ children }: ComponentPropsWithoutRef<'pre'>) => (
            <pre className="bg-muted/60 rounded-md p-4 my-3 overflow-x-auto text-sm leading-relaxed">
              {children}
            </pre>
          ),
          code({ children, className: codeClassName }) {
            const isBlock = !!codeClassName;
            if (isBlock) {
              return <code className="font-mono text-[13px]">{children}</code>;
            }
            return (
              <code className="bg-muted text-foreground rounded px-1.5 py-0.5 text-[13px] font-mono">
                {children}
              </code>
            );
          },
          table: ({
            children,
            className,
            ...props
          }: ComponentPropsWithoutRef<'table'>) => (
            <div className="overflow-x-auto my-3 rounded-md border border-border">
              <table {...props} className={cn('w-full text-sm', className)}>
                {children}
              </table>
            </div>
          ),
          thead: ({
            children,
            className,
            ...props
          }: ComponentPropsWithoutRef<'thead'>) => (
            <thead {...props} className={cn('bg-muted/40', className)}>
              {children}
            </thead>
          ),
          th: ({
            align,
            children,
            className,
            ...props
          }: ComponentPropsWithoutRef<'th'>) => (
            <th
              {...props}
              align={align}
              className={cn(
                'px-3 py-2 font-medium text-foreground/80 border-b border-border',
                markdownCellAlignClass(align),
                className,
              )}
            >
              {children}
            </th>
          ),
          td: ({
            align,
            children,
            className,
            ...props
          }: ComponentPropsWithoutRef<'td'>) => (
            <td
              {...props}
              align={align}
              className={cn(
                'px-3 py-2 border-b border-border/50',
                markdownCellAlignClass(align),
                className,
              )}
            >
              {children}
            </td>
          ),
          img: ({ src, alt }: ComponentPropsWithoutRef<'img'>) => (
            <AuthImage
              src={resolve(src)}
              alt={alt}
              className="rounded-md max-w-full my-3"
            />
          ),
          input: ({ checked, ...props }: ComponentPropsWithoutRef<'input'>) => (
            <input
              {...props}
              checked={checked}
              disabled
              className="mr-2 rounded accent-primary"
            />
          ),
        }}
      />
    </div>
  );
}
