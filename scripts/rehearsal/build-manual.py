from html import escape, unescape
from html.parser import HTMLParser
from pathlib import Path
import argparse
import re
import subprocess
parser = argparse.ArgumentParser(description="Build a standalone operator manual with Pandoc and verify its internal links.")
parser.add_argument("output", type=Path)
args = parser.parse_args()
root = Path(__file__).resolve().parents[2] / "docs"
guide = root / 'operator-guide'
home = guide / 'README.md'
titles = re.findall(r'^- \[([^]]+)\]\(([^)]+)\)', home.read_text(), re.M)
chapters = [(home, 'Operator manual')] + [((guide / target).resolve(), label) for label,target in titles]
# The landing page controls the reading order. Include deeper source registers
# and exercises in the same offline book as their referring chapters.
listed = {path for path, _ in chapters}
all_docs = set(guide.rglob("README.md")) | set((root / "operator-rehearsal").rglob("README.md"))
for path in sorted(all_docs - listed):
    heading = re.search(r"^# (.+)$", path.read_text(), re.MULTILINE)
    if not heading:
        raise ValueError(f"Chapter has no title: {path}")
    chapters.append((path, heading.group(1)))
ids = {
    path: "overview" if path == home else "-".join(path.parent.relative_to(root).parts)
    for path, _ in chapters
}
if len(set(ids.values())) != len(chapters):
    raise ValueError("Duplicate manual chapter identifier")
contents = []
for path,title in chapters:
    html = subprocess.run(['pandoc','-f','gfm','-t','html5','--wrap=none',str(path)],check=True,capture_output=True,text=True).stdout
    def fix_link(match):
        href = unescape(match[1])
        if href.startswith(('https://','http://','mailto:')): return match[0]
        target, _, frag = href.partition('#')
        dest = (path.parent / target).resolve() if target else path
        if dest not in ids: raise ValueError(href)
        anchor = ids[dest] + ('-' + frag if frag else '')
        return 'href="#' + escape(anchor, quote=True) + '"'
    html = re.sub('href="([^"]+)"',fix_link,html)
    html = re.sub('id="([^"]+)"',lambda m: 'id="' + ids[path] + '-' + m[1] + '"',html)
    contents.append(f'<section id="{ids[path]}" aria-label="{escape(title)}">{html}<a class="back" href="#contents">Contents ↑</a></section>')
toc = '\n'.join(f'<li><a href="#{ids[path]}">{escape(title)}</a></li>' for path,title in chapters)
css='''
*{box-sizing:border-box}html{scroll-padding-top:1rem}body{margin:0;color:#172b34;background:#fcfdfc;font:1rem/1.57 system-ui,sans-serif}
.wrap{max-width:86rem;margin:auto;display:grid;grid-template-columns:minmax(14rem,19rem) minmax(0,58rem);gap:clamp(1.5rem,4vw,5rem);padding:2rem min(4vw,3rem)}
nav{align-self:start;position:sticky;top:1rem;max-height:95vh;overflow:auto;border-right:1px solid #c3d2cf;padding:0 1.1rem 1rem 0}
nav h2{font-size:.96rem}nav ol{padding-left:1.45rem}nav li{margin:.36rem 0}section{padding:0 0 2.5rem;border-bottom:1px solid #b7c8c4;margin-bottom:2rem}
h1,h2,h3{line-height:1.2;text-wrap:balance}h1{font-size:clamp(1.75rem,3.0vw,2.35rem);letter-spacing:-.035em}h2{font-size:1.37rem;margin-top:1.65rem}h3{font-size:1.1rem}
a{color:#075f68;text-underline-offset:.18em}a:hover{color:#003f48}a:focus-visible{outline:3px solid #dc721e;outline-offset:3px}
table{width:100%;border-collapse:collapse;margin:1rem 0;overflow-wrap:anywhere}th,td{padding:.5rem .7rem;text-align:left;vertical-align:top;border:1px solid #c8d5d3}th{background:#eaf1ef}
code{font-family:ui-monospace,monospace;font-size:.93em;overflow-wrap:anywhere}pre{padding:1rem;background:#eef3f1;white-space:pre-wrap;border-radius:.25rem}li{margin:.25rem 0}.back{font-size:.9rem}
@media(max-width:52rem){.wrap{display:block;padding:1rem}nav{position:static;max-height:none;border-right:0;border-bottom:1px solid #c3d2cf}nav ol{columns:2}table{font-size:.92rem}}
@media print{body{background:white;font-size:10pt;color:black}.wrap{display:block;padding:0}nav{position:static;max-height:none;overflow:visible;border:0}section{break-before:page;border:0;margin:0;padding:0}.back{display:none}a{color:inherit}tr,pre{break-inside:avoid}thead{display:table-header-group}}
'''
out = args.output
out.write_text('<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1"><title>Inkue operator manual</title><style>'+css+'</style></head><body><div class="wrap"><nav id="contents" aria-label="Contents"><h2>Inkue operator manual</h2><ol>'+toc+'</ol></nav><main>'+''.join(contents)+'</main></div></body></html>')
class Check(HTMLParser):
    def __init__(self):
        super().__init__()
        self.ids, self.targets = [], []
    def handle_starttag(self,tag,attrs):
        a=dict(attrs)
        if 'id' in a:self.ids.append(a['id'])
        if 'href' in a and a['href'].startswith('#'):self.targets.append(a['href'][1:])
        assert 'src' not in a, a
check=Check()
check.feed(out.read_text())
assert len(check.ids)==len(set(check.ids))
assert set(check.targets)<=set(check.ids),set(check.targets)-set(check.ids)
print(out, out.stat().st_size,'bytes;',len(chapters),'chapters;',len(check.targets),'working internal links')
