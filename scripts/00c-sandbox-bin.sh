#!/usr/bin/env bash
# The near-workspaces build script streams the sandbox binary at about 60 KB/s from here, which
# outlives any sane timeout. This fetches the same official tarball in parallel ranges instead.
U=https://s3-us-west-1.amazonaws.com/build.nearprotocol.com/nearcore/Linux-x86_64/2.14.0/near-sandbox.tar.gz
D=/root/tools/sandbox; mkdir -p "$D"; cd "$D"
SIZE=$(timeout 20 curl -sSI "$U" | tr -d '\r' | awk 'tolower($1)=="content-length:"{print $2}')
N=24; CHUNK=$(( (SIZE + N - 1) / N ))
fetch() { i=$1; s=$((i*CHUNK)); e=$((s+CHUNK-1)); [ $e -ge $SIZE ] && e=$((SIZE-1)); want=$((e-s+1))
  for try in 1 2 3 4 5 6; do
    have=$(stat -c %s part.$i 2>/dev/null || echo 0)
    [ "$have" -ge "$want" ] && return 0
    timeout 240 curl -sS -r $((s+have))-$e "$U" >> part.$i
  done; }
for i in $(seq 0 $((N-1))); do fetch $i & done; wait
cat $(for i in $(seq 0 $((N-1))); do echo part.$i; done) > near-sandbox.tar.gz
ls -la near-sandbox.tar.gz; echo "expected $SIZE"
gzip -t near-sandbox.tar.gz && tar xzf near-sandbox.tar.gz && find . -name near-sandbox -type f -exec cp {} /root/tools/near-sandbox \;
chmod +x /root/tools/near-sandbox; /root/tools/near-sandbox --version; rm -f part.*
