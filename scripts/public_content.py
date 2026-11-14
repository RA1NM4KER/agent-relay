"""Build clean Markdown from the explicit, reviewed public HTML fragments only."""
from html.parser import HTMLParser
from dataclasses import dataclass, field
from urllib.parse import urljoin
import re


@dataclass
class Node:
    tag: str
    attrs: dict = field(default_factory=dict)
    children: list = field(default_factory=list)


class Fragment(HTMLParser):
    def __init__(self, html):
        super().__init__(convert_charrefs=True)
        self.root = Node('root')
        self.stack = [self.root]
        self.feed(html)

    def handle_starttag(self, tag, attrs):
        node = Node(tag, dict(attrs))
        self.stack[-1].children.append(node)
        if tag not in {'area', 'base', 'br', 'col', 'embed', 'hr', 'img', 'input', 'link', 'meta', 'param', 'source', 'track', 'wbr'}:
            self.stack.append(node)

    def handle_startendtag(self, tag, attrs):
        self.stack[-1].children.append(Node(tag, dict(attrs)))

    def handle_endtag(self, tag):
        for index in range(len(self.stack) - 1, 0, -1):
            if self.stack[index].tag == tag:
                del self.stack[index:]
                break

    def handle_data(self, text):
        self.stack[-1].children.append(text)


def text(node):
    return node if isinstance(node, str) else ''.join(text(child) for child in node.children)


def markdown(fragment, canonical):
    def render(node):
        if isinstance(node, str):
            return re.sub(r'\s+', ' ', node)
        if node.tag in {'script', 'style', 'button', 'svg', 'canvas'} or node.attrs.get('aria-hidden') == 'true' or 'hidden' in node.attrs or 'front-console-title' in node.attrs.get('class', '').split():
            return ''
        tag = node.tag
        if tag == 'pre':
            content = text(node)
            fence = '`' * max(3, max((len(run) + 1 for run in re.findall(r'`+', content)), default=3))
            return f'\n\n{fence}\n{content}\n{fence}\n\n'
        body = ''.join(render(child) for child in node.children)
        if 'guide-path' in node.attrs.get('class', '').split():
            return '\n\n' + '\n'.join('- ' + render(child).strip() for child in node.children if isinstance(child, Node) and child.tag == 'a') + '\n\n'
        if 'front-install-note' in node.attrs.get('class', '').split():
            return '\n\n' + ' '.join(render(child).strip() for child in node.children if isinstance(child, Node)) + '\n\n'
        if tag == 'p':
            body = re.sub(r'(\*\*[^*]+\*\*)(?=\w)', r'\1 ', body)
        if tag == 'br':
            return ' '
        if tag in {'h1', 'h2', 'h3', 'h4', 'h5', 'h6'}:
            return '\n\n' + '#' * max(2, int(tag[1])) + ' ' + body.strip() + '\n\n'
        if tag in {'p', 'figcaption', 'div', 'section', 'main', 'figure', 'details'}:
            return '\n\n' + body.strip() + '\n\n' if body.strip() else ''
        if tag == 'span':
            return body
        if tag in {'strong', 'b', 'dt', 'summary'}:
            return '**' + body.strip() + '**' + ('\n\n' if tag in {'dt', 'summary'} else '')
        if tag in {'em', 'i'}:
            return '*' + body.strip() + '*'
        if tag == 'dd':
            return body.strip() + '\n\n'
        if tag == 'code':
            value = text(node)
            fence = '`' * max(1, max((len(run) + 1 for run in re.findall(r'`+', value)), default=1))
            return fence + (' ' if '`' in value else '') + value + (' ' if '`' in value else '') + fence
        if tag == 'a':
            href = node.attrs.get('href', '')
            return f'[{body.strip()}]({urljoin(canonical, href)})' if href and body.strip() else body
        if tag == 'img':
            alt = node.attrs.get('alt', '')
            return f'![{alt}]({urljoin(canonical, node.attrs.get("src", ""))})' if alt else ''
        if tag in {'ul', 'ol'}:
            items = [child for child in node.children if isinstance(child, Node) and child.tag == 'li']
            result = []
            for index, child in enumerate(items, 1):
                value = ''.join(render(part) for part in child.children).strip()
                value = re.sub(r'^#{2,6} (.+)', r'**\1**', value)
                prefix = f'{index}. ' if tag == 'ol' else '- '
                result.append(prefix + value.replace('\n', '\n' + ' ' * len(prefix)))
            return '\n\n' + '\n'.join(result) + '\n\n'
        if tag == 'table':
            rows = []
            def collect(part):
                if not isinstance(part, Node):
                    return
                if part.tag == 'tr':
                    cells = [child for child in part.children if isinstance(child, Node) and child.tag in {'td', 'th'}]
                    rows.append([re.sub(r'\s+', ' ', ''.join(render(c) for c in cell.children)).strip().replace('|', '\\|') for cell in cells])
                else:
                    for child in part.children:
                        collect(child)
            collect(node)
            if not rows:
                return ''
            width = max(len(row) for row in rows)
            rows = [row + [''] * (width - len(row)) for row in rows]
            return '\n\n' + '\n'.join(['| ' + ' | '.join(rows[0]) + ' |', '| ' + ' | '.join(['---'] * width) + ' |'] + ['| ' + ' | '.join(row) + ' |' for row in rows[1:]]) + '\n\n'
        return body
    # Normalize prose spacing without altering fenced commands or whitespace inside them.
    raw = render(Fragment(fragment).root)
    result = []
    in_fence = False
    for line in raw.splitlines():
        if re.match(r'^`{3,}', line):
            in_fence = not in_fence
            result.append(line)
        elif in_fence:
            result.append(line)
        elif line.strip():
            result.append(line.rstrip())
        elif result and result[-1] != '':
            result.append('')
    return '\n'.join(result).strip() + '\n'
