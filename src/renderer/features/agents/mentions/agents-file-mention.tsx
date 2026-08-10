"use client"

import {
  createElement,
} from "react"
import { flushSync } from "react-dom"
import { createRoot } from "react-dom/client"
import {
  SkillIcon,
  CustomAgentIcon,
  OriginalMCPIcon,
} from "../../../components/ui/icons"
import {
  TypeScriptIcon,
  JavaScriptIcon,
  PythonIcon,
  GoIcon,
  RustIcon,
  ReactIcon,
  MarkdownInfoIcon,
  MarkdownIcon,
  CSSIcon,
  HTMLIcon,
  SCSSIcon,
  JSONIcon,
  YAMLIcon,
  ShellIcon,
  SQLIcon,
  GraphQLIcon,
  PrismaIcon,
  DockerIcon,
  TOMLIcon,
  JavaIcon,
  CIcon,
  CppIcon,
  CSharpIcon,
  PHPIcon,
  RubyIcon,
  KotlinIcon,
  VueIcon,
  SvelteIcon,
  AstroIcon,
  SwiftIcon,
  PDFIcon,
  SVGIcon,
  TxtIcon,
  GitIcon,
  NpmIcon,
  UnknownFileIcon,
} from "../../../icons/framework-icons"

// Custom folder icon matching design
function FolderOpenIcon({ className }: { className?: string }) {
  return (
    <svg
      xmlns="http://www.w3.org/2000/svg"
      viewBox="0 0 24 24"
      fill="none"
      className={className}
    >
      <path
        d="M4 8V6C4 4.89543 4.89543 4 6 4H14C15.1046 4 16 4.89543 16 6M4 8H8.17548C8.70591 8 9.21462 8.21071 9.58969 8.58579L11.4181 10.4142C11.7932 10.7893 12.3019 11 12.8323 11H16M4 8C3.44987 8 3.00391 8.44597 3.00391 8.99609V18C3.00391 19.1046 3.89934 20 5.00391 20H19.0039C20.1085 20 21.0039 19.1046 21.0039 18V12.0039C21.0039 11.4495 20.5544 11 20 11M16 11V6M16 11H20M16 6H18C19.1046 6 20 6.89543 20 8V11"
        stroke="currentColor"
        strokeWidth="2"
        strokeLinejoin="round"
      />
    </svg>
  )
}

// Get file icon component based on file extension
// If returnNullForUnknown is true, returns null for unknown file types instead of default icon
export function getFileIconByExtension(
  filename: string,
  returnNullForUnknown = false,
) {
  const filenameLower = filename.toLowerCase()

  // Special handling for files without extensions (like Dockerfile)
  if (filenameLower === "dockerfile" || filenameLower.endsWith("/dockerfile")) {
    return DockerIcon
  }

  // Special handling for dotfiles
  const baseFilename = filenameLower.split("/").pop() || filenameLower
  if (baseFilename === ".gitignore") {
    return GitIcon
  }
  if (baseFilename === ".npmrc") {
    return NpmIcon
  }
  if (baseFilename === ".prettierrc") {
    return JSONIcon
  }

  // Special handling for .env files
  // .env (without suffix) -> TOML icon
  // .env.local, .env.example, .env.development, etc. -> Shell icon
  if (baseFilename === ".env") {
    return TOMLIcon
  }
  if (baseFilename.startsWith(".env.")) {
    // .env.local, .env.example, .env.development, etc.
    return ShellIcon
  }

  // Special handling for markdown files
  // README files -> MarkdownInfoIcon (with exclamation mark)
  // Other .md/.mdx files -> MarkdownIcon (standard markdown icon)
  if (filenameLower.endsWith(".md") || filenameLower.endsWith(".mdx")) {
    const nameWithoutExt = filenameLower.replace(/\.(md|mdx)$/, "")
    if (nameWithoutExt === "readme") {
      return MarkdownInfoIcon
    }
    return MarkdownIcon
  }

  // Special handling for JavaScript files
  // Ensure .js/.mjs/.cjs files use JavaScriptIcon, not JSONIcon
  if (
    filenameLower.endsWith(".js") ||
    filenameLower.endsWith(".mjs") ||
    filenameLower.endsWith(".cjs")
  ) {
    return JavaScriptIcon
  }

  const ext = filename.split(".").pop()?.toLowerCase() || ""

  switch (ext) {
    case "tsx":
      return ReactIcon
    case "ts":
      return TypeScriptIcon
    case "js":
    case "mjs":
    case "cjs":
      return JavaScriptIcon
    case "jsx":
      return ReactIcon
    case "py":
    case "pyw":
    case "pyi":
      return PythonIcon
    case "go":
      return GoIcon
    case "rs":
      return RustIcon
    case "md":
    case "mdx":
      // This case is handled above in special handling, but kept as fallback
      // Check if it's README
      const nameWithoutExt = filenameLower.replace(/\.(md|mdx)$/, "")
      if (nameWithoutExt === "readme") {
        return MarkdownInfoIcon
      }
      return MarkdownIcon
    case "css":
      return CSSIcon
    case "html":
    case "htm":
      return HTMLIcon
    case "scss":
    case "sass":
      return SCSSIcon
    case "json":
    case "jsonc":
      return JSONIcon
    case "yaml":
    case "yml":
      return YAMLIcon
    case "sh":
    case "bash":
    case "zsh":
      return ShellIcon
    case "sql":
      return SQLIcon
    case "graphql":
    case "gql":
      return GraphQLIcon
    case "prisma":
      return PrismaIcon
    case "dockerfile":
      return DockerIcon
    case "toml":
      return TOMLIcon
    case "env":
      // This handles .env files, but we already handled them above
      // This is a fallback for edge cases
      return TOMLIcon
    case "java":
      return JavaIcon
    case "c":
    case "h":
      return CIcon
    case "cpp":
    case "cc":
    case "cxx":
    case "hpp":
      return CppIcon
    case "cs":
      return CSharpIcon
    case "php":
      return PHPIcon
    case "rb":
      return RubyIcon
    case "kt":
      return KotlinIcon
    case "vue":
      return VueIcon
    case "svelte":
      return SvelteIcon
    case "astro":
      return AstroIcon
    case "swift":
      return SwiftIcon
    case "pdf":
      return PDFIcon
    case "svg":
      return SVGIcon
    case "txt":
      return TxtIcon
    default:
      return returnNullForUnknown ? null : UnknownFileIcon
  }
}

/**
 * Create MCP icon SVG element directly via DOM API (avoids flushSync issues)
 */
function createMCPIconElement(): SVGSVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg")
  svg.setAttribute("viewBox", "0 0 24 24")
  svg.setAttribute("fill", "none")
  svg.className.baseVal = "h-3 w-3 text-muted-foreground flex-shrink-0"

  const path1 = document.createElementNS("http://www.w3.org/2000/svg", "path")
  path1.setAttribute("fill-rule", "evenodd")
  path1.setAttribute("clip-rule", "evenodd")
  path1.setAttribute("d", "M15.0915 3.8956C14.6865 3.50142 14.1437 3.28087 13.5785 3.28087C13.0133 3.28087 12.4705 3.50142 12.0655 3.8956L3.9966 11.8086C3.86157 11.9398 3.6807 12.0132 3.4924 12.0132C3.3041 12.0132 3.12322 11.9398 2.9882 11.8086C2.92209 11.7443 2.86955 11.6674 2.83366 11.5824C2.79778 11.4975 2.7793 11.4062 2.7793 11.314C2.7793 11.2218 2.79778 11.1305 2.83366 11.0456C2.86955 10.9606 2.92209 10.8837 2.9882 10.8194L11.0571 2.90647C11.732 2.24962 12.6367 1.8821 13.5785 1.8821C14.5203 1.8821 15.425 2.24962 16.0999 2.90647C16.4905 3.28628 16.7855 3.75318 16.961 4.26894C17.1364 4.7847 17.1872 5.33467 17.1092 5.87384C17.6555 5.79614 18.2124 5.84491 18.7369 6.0164C19.2614 6.18789 19.7395 6.47752 20.1344 6.86296L20.1763 6.90487C20.5068 7.22632 20.7695 7.61077 20.949 8.0355C21.1284 8.46023 21.2208 8.91661 21.2208 9.37768C21.2208 9.83874 21.1284 10.2951 20.949 10.7199C20.7695 11.1446 20.5068 11.529 20.1763 11.8505L12.8786 19.0065C12.8565 19.0279 12.839 19.0535 12.8271 19.0818C12.8151 19.1101 12.809 19.1405 12.809 19.1712C12.809 19.202 12.8151 19.2324 12.8271 19.2606C12.839 19.2889 12.8565 19.3145 12.8786 19.336L14.3773 20.8062C14.4435 20.8705 14.496 20.9474 14.5319 21.0323C14.5678 21.1173 14.5862 21.2086 14.5862 21.3008C14.5862 21.393 14.5678 21.4843 14.5319 21.5692C14.496 21.6542 14.4435 21.7311 14.3773 21.7953C14.2423 21.9266 14.0614 22 13.8731 22C13.6848 22 13.504 21.9266 13.3689 21.7953L11.8702 20.3259C11.7158 20.1759 11.5931 19.9965 11.5093 19.7982C11.4255 19.6 11.3823 19.3869 11.3823 19.1717C11.3823 18.9564 11.4255 18.7434 11.5093 18.5451C11.5931 18.3468 11.7158 18.1674 11.8702 18.0174L19.1679 10.8605C19.3661 10.6676 19.5236 10.4369 19.6312 10.1821C19.7388 9.92724 19.7942 9.65344 19.7942 9.37684C19.7942 9.10023 19.7388 8.82643 19.6312 8.5716C19.5236 8.31677 19.3661 8.08608 19.1679 7.89316L19.126 7.85208C18.7214 7.45833 18.1793 7.23779 17.6147 7.23732C17.0502 7.23685 16.5077 7.45648 16.1024 7.84957L10.0906 13.7457L10.0889 13.7474L10.0068 13.8287C9.87171 13.9602 9.69065 14.0338 9.50215 14.0338C9.31365 14.0338 9.1326 13.9602 8.99753 13.8287C8.93142 13.7644 8.87888 13.6875 8.843 13.6026C8.80712 13.5177 8.78863 13.4264 8.78863 13.3342C8.78863 13.2419 8.80712 13.1507 8.843 13.0657C8.87888 12.9808 8.93142 12.9039 8.99753 12.8396L15.094 6.86045C15.2917 6.66739 15.4487 6.43672 15.5559 6.18203C15.663 5.92735 15.7181 5.65379 15.7178 5.37749C15.7176 5.10119 15.6621 4.82773 15.5545 4.57322C15.4469 4.31872 15.2895 4.08832 15.0915 3.8956Z")
  path1.setAttribute("fill", "currentColor")
  svg.appendChild(path1)

  const path2 = document.createElementNS("http://www.w3.org/2000/svg", "path")
  path2.setAttribute("fill-rule", "evenodd")
  path2.setAttribute("clip-rule", "evenodd")
  path2.setAttribute("d", "M14.0817 5.87383C14.1478 5.80954 14.2004 5.73265 14.2362 5.64771C14.2721 5.56276 14.2906 5.47148 14.2906 5.37927C14.2906 5.28706 14.2721 5.19578 14.2362 5.11084C14.2004 5.02589 14.1478 4.949 14.0817 4.88471C13.9467 4.75322 13.7656 4.67964 13.5771 4.67964C13.3886 4.67964 13.2075 4.75322 13.0725 4.88471L7.10506 10.7373C6.77452 11.0587 6.51179 11.4432 6.33239 11.8679C6.15298 12.2926 6.06055 12.749 6.06055 13.2101C6.06055 13.6712 6.15298 14.1275 6.33239 14.5523C6.51179 14.977 6.77452 15.3615 7.10506 15.6829C7.78012 16.3396 8.68472 16.7069 9.62648 16.7069C10.5682 16.7069 11.4728 16.3396 12.1479 15.6829L18.1162 9.83032C18.1823 9.76603 18.2348 9.68914 18.2707 9.60419C18.3066 9.51925 18.3251 9.42797 18.3251 9.33576C18.3251 9.24355 18.3066 9.15227 18.2707 9.06732C18.2348 8.98238 18.1823 8.90549 18.1162 8.8412C17.9811 8.70971 17.8 8.63613 17.6115 8.63613C17.423 8.63613 17.242 8.70971 17.1069 8.8412L11.1395 14.6938C10.7345 15.088 10.1916 15.3085 9.62648 15.3085C9.06132 15.3085 8.51847 15.088 8.11346 14.6938C7.91524 14.5009 7.75769 14.2702 7.65012 14.0153C7.54254 13.7605 7.48712 13.4867 7.48712 13.2101C7.48712 12.9335 7.54254 12.6597 7.65012 12.4049C7.75769 12.15 7.91524 11.9193 8.11346 11.7264L14.0817 5.87383Z")
  path2.setAttribute("fill", "currentColor")
  svg.appendChild(path2)

  return svg
}

// Create SVG icon element in DOM based on file extension or type
export function createFileIconElement(filename: string, type?: "file" | "folder" | "skill" | "agent" | "category" | "tool"): SVGSVGElement {
  // Tool type: create MCP icon directly via DOM API (flushSync is unreliable for this icon)
  if (type === "tool") {
    return createMCPIconElement()
  }

  const IconComponent = type === "skill"
    ? SkillIcon
    : type === "agent"
      ? CustomAgentIcon
    : type === "folder"
      ? FolderOpenIcon
      : (getFileIconByExtension(filename) ?? UnknownFileIcon)
  // Note: "category" type will use the default file icon based on filename, which is fine since
  // categories won't be inserted as mentions in the editor (they navigate to subpages)

  // Create a temporary container
  const container = document.createElement("div")
  container.style.display = "none"
  container.style.position = "absolute"
  container.style.visibility = "hidden"
  document.body.appendChild(container)

  // Create React element
  const iconElement = createElement(IconComponent, {
    className: "h-3 w-3 text-muted-foreground flex-shrink-0",
  })

  const root = createRoot(container)

  // Render synchronously using flushSync
  flushSync(() => {
    root.render(iconElement)
  })

  // Extract the SVG element
  const svgElement = container.querySelector("svg")

  // Clean up
  root.unmount()
  if (container.parentNode) {
    document.body.removeChild(container)
  }

  if (!svgElement || !(svgElement instanceof SVGSVGElement)) {
    // Fallback: create unknown file icon
    const fallbackSvg = document.createElementNS(
      "http://www.w3.org/2000/svg",
      "svg",
    )
    fallbackSvg.setAttribute("width", "12")
    fallbackSvg.setAttribute("height", "12")
    fallbackSvg.setAttribute("viewBox", "0 0 24 24")
    fallbackSvg.setAttribute("fill", "none")
    fallbackSvg.setAttribute("stroke", "currentColor")
    fallbackSvg.setAttribute("stroke-width", "2")
    fallbackSvg.setAttribute("stroke-linecap", "round")
    fallbackSvg.setAttribute("stroke-linejoin", "round")
    fallbackSvg.className.baseVal =
      "h-3 w-3 text-muted-foreground flex-shrink-0"

    const paths = [
      "M17 14.5L6.0001 14.4999",
      "M11.2426 10.6215H5.99955",
      "M11.2426 18.6215H5.99955",
      "M16.7578 6.37887L5.99995 6.37896",
    ]
    for (const d of paths) {
      const path = document.createElementNS("http://www.w3.org/2000/svg", "path")
      path.setAttribute("d", d)
      fallbackSvg.appendChild(path)
    }

    return fallbackSvg
  }

  // Clone the SVG to avoid issues
  const clonedSvg = svgElement.cloneNode(true) as SVGSVGElement
  clonedSvg.setAttribute("class", "h-3 w-3 text-muted-foreground flex-shrink-0")

  return clonedSvg
}
