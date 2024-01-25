# Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Integration tests for Firecracker's huge pages support"""
import signal
import subprocess
import time

import pytest

from framework import utils
from framework.microvm import HugePagesConfig
from framework.properties import global_props
from integration_tests.functional.test_uffd import SOCKET_PATH, spawn_pf_handler


def check_hugetlbfs_in_use(pid: int, allocation_name: str):
    """Asserts that the process with the given `pid` is using hugetlbfs pages somewhere.

    `allocation_name` should be the name of the smaps entry for which we want to verify that huge pages are used.
    For memfd-backed guest memory, this would be "memfd:guest_mem", for anonymous memory this would be "/anon_hugepage"
    """

    # Format of a sample smaps entry:
    #   7fc2bc400000-7fc2cc400000 rw-s 00000000 00:10 25488401                   /memfd:guest_mem (deleted)
    #   Size:             262144 kB
    #   KernelPageSize:     2048 kB
    #   MMUPageSize:        2048 kB
    #   Rss:                   0 kB
    #   Pss:                   0 kB
    #   Pss_Dirty:             0 kB
    #   Shared_Clean:          0 kB
    #   Shared_Dirty:          0 kB
    #   Private_Clean:         0 kB
    #   Private_Dirty:         0 kB
    #   Referenced:            0 kB
    #   Anonymous:             0 kB
    #   LazyFree:              0 kB
    #   AnonHugePages:         0 kB
    #   ShmemPmdMapped:        0 kB
    #   FilePmdMapped:         0 kB
    #   Shared_Hugetlb:        0 kB
    #   Private_Hugetlb:   92160 kB
    #   Swap:                  0 kB
    #   SwapPss:               0 kB
    #   Locked:                0 kB
    #   THPeligible:           0
    #   ProtectionKey:         0
    # the "memfd:guest_mem" is the identifier of our guest memory. It is memfd backed, with the memfd being called "guest_mem" in memory.rs
    cmd = f"cat /proc/{pid}/smaps | grep {allocation_name} -A 23 | grep KernelPageSize"
    _, stdout, _ = utils.run_cmd(cmd)

    kernel_page_size_kib = int(stdout.split()[1])
    assert kernel_page_size_kib > 4


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_hugetlbfs_boot(uvm_plain):
    """Tests booting a microvm with guest memory backed by 2MB hugetlbfs pages"""

    uvm_plain.spawn()
    uvm_plain.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB, mem_size_mib=128)
    uvm_plain.add_net_iface()
    uvm_plain.start()

    rc, _, _ = uvm_plain.ssh.run("true")
    assert not rc

    check_hugetlbfs_in_use(uvm_plain.firecracker_pid, "memfd:guest_mem")


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_hugetlbfs_snapshot(
    microvm_factory, guest_kernel_linux_5_10, rootfs_ubuntu_22, uffd_handler_paths
):
    """
    Test hugetlbfs snapshot restore via uffd
    """

    ### Create Snapshot ###
    utils.run_cmd("apt-get update && apt-get -y install trace-cmd")

    vm = microvm_factory.build(guest_kernel_linux_5_10, rootfs_ubuntu_22)
    vm.memory_monitor = None
    vm.jailer.daemonize = False
    vm.jailer.extra_args.update({"no-seccomp": None})
    vm.spawn()
    proc = subprocess.Popen(
        f"trace-cmd record -c -e kvm -P {vm.firecracker_pid}".split()
    )
    # give trace-cmd time to initialize
    time.sleep(10)
    vm.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB, mem_size_mib=128)
    vm.add_net_iface()
    vm.start()

    # Wait for microvm to boot
    rc, _, _ = vm.ssh.run("true")
    assert not rc

    # check_hugetlbfs_in_use(vm.firecracker_pid, "memfd:guest_mem")

    snapshot = vm.snapshot_full()

    proc.send_signal(signal.SIGINT)
    proc.wait()
    print("waited")
    print(utils.run_cmd(f"/proc/{vm.firecracker_pid}/statm"))
    vm.kill()
    print("killed")

    print(utils.run_cmd(f"trace-cmd report").stdout)

    return

    ### Restore Snapshot ###
    vm = microvm_factory.build()
    vm.spawn()

    # Spawn page fault handler process.
    _pf_handler = spawn_pf_handler(
        vm, uffd_handler_paths["valid_4k_handler"], snapshot.mem
    )

    vm.restore_from_snapshot(snapshot, resume=True, uffd_path=SOCKET_PATH)

    # Verify if guest can run commands.
    rc, _, _ = vm.ssh.run("true")
    assert not rc

    # check_hugetlbfs_in_use(vm.firecracker_pid, "/anon_hugepage")


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_negative_huge_pages_plus_balloon(uvm_plain):
    """Tests that huge pages and memory ballooning cannot be used together"""
    uvm_plain.memory_monitor = None
    uvm_plain.spawn()

    # Ensure setting huge pages and then adding a balloon device doesn't work
    uvm_plain.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB)
    with pytest.raises(
        RuntimeError,
        match="Firecracker's huge pages support is incompatible with memory ballooning.",
    ):
        uvm_plain.api.balloon.put(amount_mib=0, deflate_on_oom=False)

    # Ensure adding a balloon device and then setting huge pages doesn't work
    uvm_plain.basic_config(huge_pages=HugePagesConfig.NONE)
    uvm_plain.api.balloon.put(amount_mib=0, deflate_on_oom=False)
    with pytest.raises(
        RuntimeError,
        match="Machine config error: Firecracker's huge pages support is incompatible with memory ballooning.",
    ):
        uvm_plain.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB)


@pytest.mark.skipif(
    global_props.host_linux_version == "4.14",
    reason="MFD_HUGETLB | MFD_ALLOW_SEALING only supported on kernels >= 4.16",
)
def test_negative_huge_pages_plus_initrd(uvm_with_initrd):
    """Tests that huge pages and initrd cannot be used together"""
    uvm_with_initrd.jailer.daemonize = False
    uvm_with_initrd.spawn()
    uvm_with_initrd.memory_monitor = None

    # Ensure setting huge pages and then telling FC to boot an initrd does not work
    with pytest.raises(
        RuntimeError,
        match="Boot source error: Firecracker's huge pages support is incompatible with initrds.",
    ):
        # `basic_config` first does a PUT to /machine-config, which will apply the huge pages configuration,
        # and then a PUT to /boot-source, which will register the initrd
        uvm_with_initrd.basic_config(
            boot_args="console=ttyS0 reboot=k panic=1 pci=off",
            use_initrd=True,
            huge_pages=HugePagesConfig.HUGETLBFS_2MB,
            add_root_device=False,
            vcpu_count=1,
        )

    # Ensure telling FC about the initrd first and then setting huge pages doesn't work
    # This first does a PUT to /machine-config to reset the huge pages configuration, before doing a
    # PUT to /boot-source to register the initrd
    uvm_with_initrd.basic_config(
        huge_pages=HugePagesConfig.NONE,
        boot_args="console=ttyS0 reboot=k panic=1 pci=off",
        use_initrd=True,
    )
    with pytest.raises(
        RuntimeError,
        match="Machine config error: Firecracker's huge pages support is incompatible with initrds.",
    ):
        # This does a PUT /machine-config to update the huge pages config, which will fail because
        # the init rd was configured by the previous call to `basic_config`.
        uvm_with_initrd.basic_config(huge_pages=HugePagesConfig.HUGETLBFS_2MB)
