"""Send mDNS discovery on each IPv4 interface on Windows.

The dependency's ordinary multicast discovery is retained. Extra, interface-bound
queries use unicast responses so an unrelated VPN route or another mDNS listener
does not silently hide a local iPhone.
"""
from __future__ import annotations

import asyncio
import socket
import struct
import sys


_enabled = False


def enable_windows_discovery() -> None:
    global _enabled
    if _enabled or sys.platform != "win32":
        return
    import ifaddr
    from pymobiledevice3 import bonjour

    open_sockets = bonjour._open_mdns_sockets
    send_query = bonjour._send_query_all

    async def open_for_interfaces():
        transports, queue = await open_sockets()
        loop = asyncio.get_running_loop()
        seen = set()
        for adapter in ifaddr.get_adapters():
            for candidate in adapter.ips:
                ip = candidate.ip
                if not isinstance(ip, str) or ip.startswith(("127.", "169.254.", "0.")) or ip in seen:
                    continue
                seen.add(ip)
                sock = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
                try:
                    sock.bind((ip, 0))
                    sock.setsockopt(socket.IPPROTO_IP, socket.IP_MULTICAST_IF, socket.inet_aton(ip))
                    transport, _ = await loop.create_datagram_endpoint(
                        lambda: bonjour._DatagramProtocol(queue), sock=sock
                    )
                    transports.append((transport, sock))
                except OSError:
                    sock.close()
        return transports, queue

    async def send_on_interfaces(transports, packet, skip_interfaces=()):
        sent = await send_query(transports, packet, skip_interfaces)
        if len(packet) < 2:
            return sent
        query_class = struct.unpack("!H", packet[-2:])[0] | bonjour.CLASS_QU
        unicast_query = packet[:-2] + struct.pack("!H", query_class)
        for transport, sock in transports:
            if sock.family != socket.AF_INET or sock.getsockname()[0] == "0.0.0.0":
                continue
            try:
                transport.sendto(unicast_query, (bonjour.MDNS_MCAST_V4, bonjour.MDNS_PORT))
                sent += 1
            except OSError:
                pass
        return sent

    bonjour._open_mdns_sockets = open_for_interfaces
    bonjour._send_query_all = send_on_interfaces
    _enabled = True
