# udp-over-https

A proxy that does something neither efficient nor clean: proxy udp packets over a https-connection. But could be useful in some cases

```cmd
cargo run -- server --udp-port 9898 --http-port 8888 --udp-port-relay-target "127.0.0.1:7777"
cargo run -- client --udp-port 8989 --http-server "http://127.0.0.1:8888"

# listen on relay target, udp packet into client
nc -u -l 7777
echo "Hello, server!" | nc -u 127.0.0.1 8989

# udp packet into server (comes out of the last udp port source of client, which can be seen in its logs)
nc -u -l #here-proper-port
echo "Hello, client!" | nc -u 127.0.0.1 9898
```
