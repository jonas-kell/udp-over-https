# Tunnel: udp-over-http(s)

A proxy that does something neither efficient nor clean: proxy udp packets over a https-connection. But could be useful in some cases

## Developing and local testing

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

## Use with wireguard

Compile with the `--release` flag!

Can be used with standard wireguard to get a vpn that runs undetectable fully over https.
However you must make sure, that the tunnel http(s) traffic doesn't get routed over the vpn, or it will get stuck (obviously).

All commands might need `sudo`!

After starting wireguard, exclude the endpoint via direct route and start the executable in client mode.

```cmd
ip route add $(dig +short A your.vpn.domain)/32 via $(ip route | grep default | awk '{print $3}')
/path/to/exe/udp-over-https client --udp-port 9876 --http-server "http(s)://your.vpn.domain" --keep-alive-ms 300
```

After shutting down, revert this change (optional) and shutdown the client.

```cmd
ip route del $(dig +short A your.vpn.domain)/32 via $(ip route | grep default | awk '{print $3}')
fuser -k 9876/udp
```

## Server config

Potentially ssl-terminate the server with your own solution.

Start the server as a service.

`~/udp-over-http.sh`

```bash
#!/bin/bash

EXECUTABLE="/path/to/your/executable"
PARAMS="server --udp-port-relay-target 127.0.0.1:51820 --http-port 9877 --udp-port 9866"

# Change to the directory of the executable (optional)
cd "$(dirname "$EXECUTABLE")" || exit 1

# Start the executable
"$EXECUTABLE" $PARAMS
```

```cmd
sudo nano /etc/systemd/system/udp_over_http.service

# insert service file content

sudo systemctl daemon-reload
sudo systemctl enable udp_over_http.service
sudo systemctl start udp_over_http.service

# see it running
sudo systemctl status udp_over_http.service
```

`/etc/systemd/system/udp_over_http.service`

```conf
[Unit]
Description=UDP over HTTP tunnel service
After=network.target

[Service]
Type=simple
ExecStart=/root/udp-over-http.sh
Restart=always
User=root
WorkingDirectory=/root

[Install]
WantedBy=multi-user.target
```

## Security

The tunnel can be encrypted with ssl and then theoretically is secure. That is however not the goal of this program and so I am sure, that there are MANY potential security risks.
Use wireguard for the security and use this ONLY as a transport solution.

SOME design choices have been made to avoid security problems, but no promises.

```cmd
openssl rand -hex 32 # put into --pre-shared-secret
```
