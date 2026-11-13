import ReactMarkdown from "react-markdown";
import { api } from "./api";

/** 不让 javascript: / data: / vbscript: 这类链接带着地址出现在页面里（点击由我们自己处理，也不会真的跳转） */
const BAD = /^\s*(javascript|data|vbscript):/i;

/**
 * 回答的 Markdown 渲染。链接一律由我们接管：
 *  - http(s) 链接 → 用系统默认浏览器打开
 *  - 本地路径（C:/xxx、C:\xxx、/Users/xxx、file:///xxx）→ 用默认程序打开；可执行文件只在资源管理器 / 访达里定位，不会运行
 * （WebView 自己不会处理这类链接，直接点击没有任何反应。）
 */
export default function Md({ children }: { children: string }) {
  return (
    <ReactMarkdown
      urlTransform={(u) => (BAD.test(u) ? "" : u)}
      components={{
        a: ({ href, children: label }) => (
          <a
            href={href || undefined}
            title={href}
            style={{ cursor: "pointer" }}
            onClick={(e) => {
              e.preventDefault();
              e.stopPropagation();
              if (href) void api.openPath(href).catch(() => {});
            }}
          >
            {label}
          </a>
        ),
      }}
    >
      {children}
    </ReactMarkdown>
  );
}
