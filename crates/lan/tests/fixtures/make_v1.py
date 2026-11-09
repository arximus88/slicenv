# Робить із v3-сертифіката v1: прибирає [0] version і [3] extensions, переподписує.
import sys, subprocess, base64
def rd(b, i):
    tag = b[i]; l = b[i+1]; i2 = i+2
    if l & 0x80:
        n = l & 0x7f; l = int.from_bytes(b[i2:i2+n], 'big'); i2 += n
    return tag, i2, i2+l
def enc(tag, body):
    n = len(body)
    if n < 128: L = bytes([n])
    else:
        nb = n.to_bytes((n.bit_length()+7)//8, 'big'); L = bytes([0x80|len(nb)]) + nb
    return bytes([tag]) + L + body
def children(b, s, e):
    out = []; i = s
    while i < e:
        t, cs, ce = rd(b, i); out.append((t, b[i:ce])); i = ce
    return out
pem, key, out = sys.argv[1:4]
der = subprocess.run(['openssl','x509','-in',pem,'-outform','DER'],capture_output=True,check=True).stdout
_, cs, ce = rd(der, 0)
parts = children(der, cs, ce)
tbs_t, tbs_raw = parts[0]; alg = parts[1][1]
_, ts, te = rd(tbs_raw, 0)
fields = [f for t, f in children(tbs_raw, ts, te) if t not in (0xA0, 0xA3)]
tbs = enc(0x30, b''.join(fields))
sig = subprocess.run(['openssl','dgst','-sha256','-sign',key],input=tbs,capture_output=True,check=True).stdout
cert = enc(0x30, tbs + alg + enc(0x03, b'\x00' + sig))
b64 = base64.encodebytes(cert).decode().replace('\n','')
with open(out,'w') as f:
    f.write('-----BEGIN CERTIFICATE-----\n' + '\n'.join(b64[i:i+64] for i in range(0,len(b64),64)) + '\n-----END CERTIFICATE-----\n')
