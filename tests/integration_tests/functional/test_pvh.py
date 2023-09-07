# Copyright 2023 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0

"""Tests for PVH boot mode"""
import pytest

from framework.microvm import Serial

# pylint:disable=redefined-outer-name


def test_linux_pvh_boot(uvm_pvh):
    """
    Tests booting a PVH-enabled linux kernel for supported guest kernel version 5.10 and newer (as non-XEN PVH
    support was added to linux in 5.0).

    Asserts that the 'Kernel loaded using PVH boot protocol' log message is present
    """
    uvm_pvh.spawn()
    uvm_pvh.basic_config()
    uvm_pvh.add_net_iface()
    uvm_pvh.start()

    uvm_pvh.ssh.run("true")

    uvm_pvh.check_log_message("Kernel loaded using PVH boot protocol")


@pytest.fixture
def uvm_freebsd(microvm_factory, artifact_dir):
    """Create a FreeBSD microVM"""

    # Cant use the rootfs_fxt and guest_kernel_fxt, because they only allow us to get supported kernels (and I am
    # reluctant to add a freebsd regex to the list of supported kernels)
    return microvm_factory.build(
        artifact_dir / "freebsd/freebsd-kern.bin",
        artifact_dir / "freebsd/freebsd-rootfs.bin",
    )


def test_freebsd_pvh_boot(uvm_freebsd):
    """Tries to boot a FreeBSD microVM"""
    uvm_freebsd.jailer.daemonize = False
    uvm_freebsd.spawn()
    uvm_freebsd.basic_config(
        boot_args="vfs.root.mountfrom=ufs:/dev/vtbd0 -Dh"
    )  # -Dh for enabling serial console
    uvm_freebsd.start()

    # The FreeBSD rootfs does not contain SSH keys, so we cannot verify whether the VM booted successfully by just
    # SSH-ing into it. Therefore, we use the serial console and look for the login-prompt.
    serial = Serial(uvm_freebsd)
    serial.open()
    serial.rx("login: ")
