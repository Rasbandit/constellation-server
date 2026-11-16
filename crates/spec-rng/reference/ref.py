"""Independent reference for the spec-rng golden vectors. Standard library only: python3 -I ref.py.
It must never import or call the Rust crate; that independence is the point (docs/TESTING.md, rule 6)."""
import hmac, hashlib, json
M=0xFFFFFFFF
def rotl(v,c): return ((v<<c)&M)|(v>>(32-c))
def qr(s,a,b,c,d):
    s[a]=(s[a]+s[b])&M; s[d]=rotl(s[d]^s[a],16)
    s[c]=(s[c]+s[d])&M; s[b]=rotl(s[b]^s[c],12)
    s[a]=(s[a]+s[b])&M; s[d]=rotl(s[d]^s[a],8)
    s[c]=(s[c]+s[d])&M; s[b]=rotl(s[b]^s[c],7)
def block(key,ctr):
    k=[int.from_bytes(key[i*4:i*4+4],'little') for i in range(8)]
    st=[0x61707865,0x3320646e,0x79622d32,0x6b206574]+k+[ctr&M,(ctr>>32)&M,0,0]
    w=st[:]
    for _ in range(4):
        qr(w,0,4,8,12);qr(w,1,5,9,13);qr(w,2,6,10,14);qr(w,3,7,11,15)
        qr(w,0,5,10,15);qr(w,1,6,11,12);qr(w,2,7,8,13);qr(w,3,4,9,14)
    return [(w[i]+st[i])&M for i in range(16)]
class Rng:
    def __init__(s,seed): s.key=seed; s.words=[]; s.ctr=0
    def u32(s):
        if not s.words: s.words=block(s.key,s.ctr); s.ctr+=1
        return s.words.pop(0)
    def u64(s):
        lo=s.u32(); hi=s.u32(); return lo|(hi<<32)
    def below(s,n):
        t=(1<<64)%n
        while True:
            x=s.u64()
            if x>=t: return x%n
    def shuffle(s,a):
        for i in range(len(a)-1,0,-1):
            j=s.below(i+1); a[i],a[j]=a[j],a[i]
    def weighted(s,w):
        tot=sum(w)
        if tot==0 or tot>=1<<64: return None
        r=s.below(tot); c=0
        for i,x in enumerate(w):
            c+=x
            if c>r: return i
def hkdf(ikm,salt,info,l=32):
    prk=hmac.new(salt,ikm,hashlib.sha256).digest()
    return hmac.new(prk,info+b'\x01',hashlib.sha256).digest()[:l]
def derive(parent,label,idx): return hkdf(parent,b'cwp-rng-v1',label+b'\x00'+idx.to_bytes(4,'big'))
z=bytes(32); inc=bytes(range(32))
# sanity: RFC 7539-style ChaCha20 would differ; ChaCha8 zero-key block 0 first words
out={}
out['zero_u64']=[hex(Rng(z).u64()) for _ in range(1)]
r=Rng(z); out['zero_u64']=[hex(r.u64()) for _ in range(4)]
r=Rng(inc); out['inc_u64']=[hex(r.u64()) for _ in range(4)]
r=Rng(inc); out['inc_below_10']=[r.below(10) for _ in range(8)]
r=Rng(inc); out['inc_below_big']=[r.below((1<<63)+5) for _ in range(4)]
a=list(range(10)); r=Rng(inc); r.shuffle(a); out['inc_shuffle10']=a
r=Rng(z); out['zero_weighted']=[r.weighted([1,0,3,6]) for _ in range(8)]
out['derive_slot0']=derive(inc,b'slot',0).hex()
out['derive_slot1']=derive(inc,b'slot',1).hex()
out['derive_hook']=derive(derive(inc,b'slot',1),b'hook',7).hex()
# crosses a 16-word block boundary: 40 u64 = 80 u32
r=Rng(inc); v=[r.u64() for _ in range(40)]; out['inc_u64_idx39']=hex(v[39])
print(json.dumps(out,indent=1))
