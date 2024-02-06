# Copyright 2018 Amazon.com, Inc. or its affiliates. All Rights Reserved.
# SPDX-License-Identifier: Apache-2.0
"""Test that the process startup time up to socket bind is within spec."""

import os
import time

import pytest

from framework.properties import global_props
from host_tools.cargo_build import run_seccompiler_bin


def record_startup_time(metrics, startup_time, suffix: str):
    metrics.put_metric(f"startup_time_{suffix}", startup_time, unit="Microseconds")


def test_startup_time_new_pid_ns(
    microvm_factory, rootfs, guest_kernel_linux_5_10, metrics
):
    """
    Check startup time when jailer is spawned in a new PID namespace.
    """
    for _ in range(10):
        microvm = microvm_factory.build(guest_kernel_linux_5_10, rootfs)
        microvm.jailer.new_pid_ns = True
        record_startup_time(
            metrics,
            _test_startup_time(microvm, metrics, "test_startup_time_new_pid_ns"),
            "new_pid_ns",
        )


def test_startup_time_daemonize(
    microvm_factory, rootfs, guest_kernel_linux_5_10, metrics
):
    """
    Check startup time when jailer detaches Firecracker from the controlling terminal.
    """
    for _ in range(10):
        microvm = microvm_factory.build(guest_kernel_linux_5_10, rootfs)
        record_startup_time(
            metrics,
            _test_startup_time(microvm, metrics, "test_startup_time_daemonize"),
            "daemonize",
        )


def test_startup_time_custom_seccomp(
    microvm_factory, rootfs, guest_kernel_linux_5_10, metrics
):
    """
    Check the startup time when using custom seccomp filters.
    """
    for _ in range(10):
        microvm = microvm_factory.build(guest_kernel_linux_5_10, rootfs)
        _custom_filter_setup(microvm)
        record_startup_time(
            metrics,
            _test_startup_time(microvm, metrics, "test_startup_time_custom_seccomp"),
            "custom_seccomp",
        )


def _test_startup_time(microvm, metrics, test):
    microvm.spawn()
    microvm.basic_config(vcpu_count=2, mem_size_mib=1024)
    metrics.set_dimensions({"performance_test": test, **microvm.dimensions})
    test_start_time = time.time()
    microvm.start()
    time.sleep(0.4)

    # The metrics should be at index 1.
    # Since metrics are flushed at InstanceStart, the first line will suffice.
    datapoints = microvm.get_all_metrics()
    test_end_time = time.time()
    metrics = datapoints[0]
    startup_time_us = metrics["api_server"]["process_startup_time_us"]
    cpu_startup_time_us = metrics["api_server"]["process_startup_time_cpu_us"]

    print(
        "Process startup time is: {} us ({} CPU us)".format(
            startup_time_us, cpu_startup_time_us
        )
    )

    assert cpu_startup_time_us > 0
    # Check that startup time is not a huge value
    # This is to catch issues like the ones introduced in PR
    # https://github.com/firecracker-microvm/firecracker/pull/4305
    test_time_delta_us = (test_end_time - test_start_time) * 1000 * 1000
    assert startup_time_us < test_time_delta_us
    assert cpu_startup_time_us < test_time_delta_us
    return cpu_startup_time_us


def _custom_filter_setup(test_microvm):
    bpf_path = os.path.join(test_microvm.path, "bpf.out")

    run_seccompiler_bin(bpf_path)

    test_microvm.create_jailed_resource(bpf_path)
    test_microvm.jailer.extra_args.update({"seccomp-filter": "bpf.out"})
