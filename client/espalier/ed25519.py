"""Ed25519 from RFC 8032, in plain Python, so the client needs nothing but the standard library.
Slow and not constant time: fine for an agent signing a few transactions on its own machine, not for
a server handling other people's keys. Tested against the RFC vectors in client/tests/."""
import hashlib

P = 2**255 - 19
L = 2**252 + 27742317777372353535851937790883648493
D = -121665 * pow(121666, P - 2, P) % P
I = pow(2, (P - 1) // 4, P)


def _sha512(b):
    return hashlib.sha512(b).digest()


def _recover_x(y, sign):
    x2 = (y * y - 1) * pow(D * y * y + 1, P - 2, P) % P
    if x2 == 0:
        return None if sign else 0
    x = pow(x2, (P + 3) // 8, P)
    if (x * x - x2) % P:
        x = x * I % P
    if (x * x - x2) % P:
        return None
    return P - x if (x & 1) != sign else x


_BY = 4 * pow(5, P - 2, P) % P
_BX = _recover_x(_BY, 0)
B = (_BX, _BY, 1, _BX * _BY % P)
ZERO = (0, 1, 1, 0)


def _add(p, q):
    a = (p[1] - p[0]) * (q[1] - q[0]) % P
    b = (p[1] + p[0]) * (q[1] + q[0]) % P
    c = 2 * p[3] * q[3] * D % P
    d = 2 * p[2] * q[2] % P
    e, f, g, h = b - a, d - c, d + c, b + a
    return (e * f % P, g * h % P, f * g % P, e * h % P)


def _mul(s, p):
    q = ZERO
    while s:
        if s & 1:
            q = _add(q, p)
        p = _add(p, p)
        s >>= 1
    return q


def _compress(p):
    zi = pow(p[2], P - 2, P)
    x, y = p[0] * zi % P, p[1] * zi % P
    return int.to_bytes(y | ((x & 1) << 255), 32, "little")


def _decompress(b):
    y = int.from_bytes(b, "little")
    sign = y >> 255
    y &= (1 << 255) - 1
    if y >= P:
        return None
    x = _recover_x(y, sign)
    return None if x is None else (x, y, 1, x * y % P)


def _expand(seed):
    h = _sha512(seed)
    a = int.from_bytes(h[:32], "little")
    a &= (1 << 254) - 8
    a |= 1 << 254
    return a, h[32:]


def public_key(seed):
    return _compress(_mul(_expand(seed)[0], B))


def sign(seed, message):
    a, prefix = _expand(seed)
    pk = _compress(_mul(a, B))
    r = int.from_bytes(_sha512(prefix + message), "little") % L
    rs = _compress(_mul(r, B))
    k = int.from_bytes(_sha512(rs + pk + message), "little") % L
    return rs + int.to_bytes((r + k * a) % L, 32, "little")


def verify(pk, message, signature):
    if len(signature) != 64 or len(pk) != 32:
        return False
    a, r = _decompress(pk), _decompress(signature[:32])
    s = int.from_bytes(signature[32:], "little")
    if a is None or r is None or s >= L:
        return False
    k = int.from_bytes(_sha512(signature[:32] + pk + message), "little") % L
    left = _mul(s, B)
    right = _add(r, _mul(k, a))
    return _compress(left) == _compress(right)
