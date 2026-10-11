#!/usr/bin/env python3
"""Set the kernel's TAI-UTC offset (CLOCK_TAI - CLOCK_REALTIME), default 37 s.

Hosts without ntpd/chrony leap-second data keep the offset at 0, so CLOCK_TAI
equals UTC and every TAI time is 37 s off. NMOS controllers (and the AMWA
testing tool) compute TAI as UTC + 37 s. Needs CAP_SYS_TIME (run with sudo).

    sudo ci/set-tai-offset.py [seconds]
"""
import ctypes
import ctypes.util
import sys
import time

ADJ_TAI = 0x0080


class Timeval(ctypes.Structure):
    _fields_ = [("tv_sec", ctypes.c_long), ("tv_usec", ctypes.c_long)]


class Timex(ctypes.Structure):
    _fields_ = [
        ("modes", ctypes.c_uint),
        ("offset", ctypes.c_long),
        ("freq", ctypes.c_long),
        ("maxerror", ctypes.c_long),
        ("esterror", ctypes.c_long),
        ("status", ctypes.c_int),
        ("constant", ctypes.c_long),
        ("precision", ctypes.c_long),
        ("tolerance", ctypes.c_long),
        ("time", Timeval),
        ("tick", ctypes.c_long),
        ("ppsfreq", ctypes.c_long),
        ("jitter", ctypes.c_long),
        ("shift", ctypes.c_int),
        ("stabil", ctypes.c_long),
        ("jitcnt", ctypes.c_long),
        ("calcnt", ctypes.c_long),
        ("errcnt", ctypes.c_long),
        ("stbcnt", ctypes.c_long),
        ("tai", ctypes.c_int),
        ("padding", ctypes.c_int * 11),
    ]


def main():
    offset = int(sys.argv[1]) if len(sys.argv) > 1 else 37
    libc = ctypes.CDLL(ctypes.util.find_library("c"), use_errno=True)
    tx = Timex()
    tx.modes = ADJ_TAI
    tx.constant = offset
    if libc.adjtimex(ctypes.byref(tx)) < 0:
        err = ctypes.get_errno()
        sys.exit(f"adjtimex(ADJ_TAI) failed: errno {err} (needs CAP_SYS_TIME)")
    measured = time.clock_gettime(time.CLOCK_TAI) - time.clock_gettime(time.CLOCK_REALTIME)
    print(f"CLOCK_TAI - CLOCK_REALTIME = {measured:.3f} s")
    if round(measured) != offset:
        sys.exit(f"the kernel did not take the offset {offset}")


if __name__ == "__main__":
    main()
