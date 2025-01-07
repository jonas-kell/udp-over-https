# udp-over-https

A proxy that does something neither efficient nor clean: proxy udp packets over a https-connection. But could be useful in some cases

```cmd
echo "Hello, world!" | nc -u 127.0.0.1 8080

nc -u -l 9090
```
