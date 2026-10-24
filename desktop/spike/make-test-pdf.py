"""生成一个 2 页的最小真实 PDF（无需第三方库）。

每页画一个矩形边框 + 大号标题文字（用 WinAnsi 编码直接写文本流），
作为 markupPdf 按页渲染标注闭环的附件 fixture。
"""
import sys

def page_stream(label: str) -> bytes:
    return (
        "BT /F1 36 Tf 90 700 Td (Yinxiang PDF Fixture) Tj ET\n"
        "BT /F1 20 Tf 90 650 Td (" + label + ") Tj ET\n"
        "0.6 w 72 72 468 648 re S\n"
        "BT /F1 12 Tf 90 80 Td (Generated for markupPdf closed-loop verification) Tj ET\n"
    ).encode("latin-1")

def build_pdf(pages: int) -> bytes:
    objs = []  # (obj_num, body)
    kids = " ".join(f"{3 + i} 0 R" for i in range(pages))
    objs.append((1, b"<< /Type /Catalog /Pages 2 0 R >>"))
    objs.append((2, f"<< /Type /Pages /Kids [{kids}] /Count {pages} >>".encode()))
    for i in range(pages):
        stream = page_stream(f"Page {i + 1} of {pages}")
        objs.append((3 + i, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] "
                    b"/Resources << /Font << /F1 " + str(3 + pages).encode() + b" 0 R >> >> "
                    b"/Contents " + str(4 + pages + i).encode() + b" 0 R >>"))
        objs.append((4 + pages + i, b"<< /Length " + str(len(stream)).encode() + b" >>\nstream\n"
                    + stream + b"endstream"))
    objs.append((3 + pages, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>"))

    out = bytearray(b"%PDF-1.4\n%\xe2\xe3\xcf\xd3\n")
    offsets = {}
    for num, body in objs:
        offsets[num] = len(out)
        out += f"{num} 0 obj\n".encode() + body + b"\nendobj\n"
    xref_at = len(out)
    maxnum = max(offsets) + 1
    out += f"xref\n0 {maxnum}\n".encode()
    out += b"0000000000 65535 f \n"
    for n in range(1, maxnum):
        out += f"{offsets[n]:010d} 00000 n \n".encode()
    out += (f"trailer\n<< /Size {maxnum} /Root 1 0 R >>\nstartxref\n{xref_at}\n%%EOF\n").encode()
    return bytes(out)

if __name__ == "__main__":
    path = sys.argv[1] if len(sys.argv) > 1 else "test-doc.pdf"
    n = int(sys.argv[2]) if len(sys.argv) > 2 else 2
    with open(path, "wb") as f:
        f.write(build_pdf(n))
    print(f"wrote {path} ({n} pages)")
