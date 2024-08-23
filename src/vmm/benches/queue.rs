// Copyright 2024 Amazon.com, Inc. or its affiliates. All Rights Reserved.
// SPDX-License-Identifier: Apache-2.0
//
// Benchmarking cases:
//   * `Queue.pop`
//   * `Queue.add_used`
//   * `DescriptorChain.next_descriptor`

use std::num::Wrapping;

use criterion::{criterion_group, criterion_main, Criterion};
use vm_memory::GuestAddress;
use vmm::devices::virtio::test_utils::VirtQueue;
use vmm::utilities::test_utils::single_region_mem;

pub fn queue_benchmark(c: &mut Criterion) {
    let mem = single_region_mem(2 * 65562);
    let rxq = VirtQueue::new(GuestAddress(0), &mem, 256);
    let mut queue = rxq.create_queue();

    rxq.dtable[0].set(2048, 65562, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.idx.set(1);

    c.bench_function("next_descriptor_1", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            // SAFETY: queue has 1 desc chanin
            let desc = unsafe { queue.pop(&mem).unwrap_unchecked() };
            let mut head = Some(desc);
            while let Some(d) = head {
                head = d.next_descriptor();
            }
        })
    });

    rxq.dtable[0].set(2048, 65562 / 2, 0x3, 1);
    rxq.dtable[1].set(2048 + 65562 / 2, 65562 / 2, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.idx.set(2);

    c.bench_function("next_descriptor_2", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            // SAFETY: queue has 1 desc chanin
            let desc = unsafe { queue.pop(&mem).unwrap_unchecked() };
            let mut head = Some(desc);
            while let Some(d) = head {
                head = d.next_descriptor();
            }
        })
    });

    rxq.dtable[0].set(2048, 65562 / 4, 0x3, 1);
    rxq.dtable[1].set(2048 + (65562 / 4), 65562 / 4, 0x3, 2);
    rxq.dtable[2].set(2048 + (65562 / 4) * 2, 65562 / 4, 0x3, 3);
    rxq.dtable[3].set(2048 + (65562 / 4) * 3, 65562 / 4, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.idx.set(4);

    c.bench_function("next_descriptor_4", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            // SAFETY: queue has 1 desc chanin
            let desc = unsafe { queue.pop(&mem).unwrap_unchecked() };
            let mut head = Some(desc);
            while let Some(d) = head {
                head = d.next_descriptor();
            }
        })
    });

    rxq.dtable[0].set(2048, 65562 / 16, 0x3, 1);
    rxq.dtable[1].set(2048 + (65562 / 16), 65562 / 16, 0x3, 2);
    rxq.dtable[2].set(2048 + (65562 / 16) * 2, 65562 / 16, 0x3, 3);
    rxq.dtable[3].set(2048 + (65562 / 16) * 3, 65562 / 16, 0x3, 4);
    rxq.dtable[4].set(2048 + (65562 / 16) * 4, 65562 / 16, 0x3, 5);
    rxq.dtable[5].set(2048 + (65562 / 16) * 5, 65562 / 16, 0x3, 6);
    rxq.dtable[6].set(2048 + (65562 / 16) * 6, 65562 / 16, 0x3, 7);
    rxq.dtable[7].set(2048 + (65562 / 16) * 7, 65562 / 16, 0x3, 8);
    rxq.dtable[8].set(2048 + (65562 / 16) * 8, 65562 / 16, 0x3, 9);
    rxq.dtable[9].set(2048 + (65562 / 16) * 9, 65562 / 16, 0x3, 10);
    rxq.dtable[10].set(2048 + (65562 / 16) * 10, 65562 / 16, 0x3, 11);
    rxq.dtable[11].set(2048 + (65562 / 16) * 11, 65562 / 16, 0x3, 12);
    rxq.dtable[12].set(2048 + (65562 / 16) * 12, 65562 / 16, 0x3, 13);
    rxq.dtable[13].set(2048 + (65562 / 16) * 13, 65562 / 16, 0x3, 14);
    rxq.dtable[14].set(2048 + (65562 / 16) * 14, 65562 / 16, 0x3, 15);
    rxq.dtable[15].set(2048 + (65562 / 16) * 15, 65562 / 16, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.idx.set(16);

    c.bench_function("next_descriptor_16", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            // SAFETY: queue has 1 desc chanin
            let desc = unsafe { queue.pop(&mem).unwrap_unchecked() };
            let mut head = Some(desc);
            while let Some(d) = head {
                head = d.next_descriptor();
            }
        })
    });

    // Queue pop

    rxq.dtable[0].set(2048, 65562, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.idx.set(1);

    c.bench_function("queue_pop_1", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            while let Some(desc) = queue.pop(&mem) {
                std::hint::black_box(desc);
            }
        })
    });

    rxq.dtable[0].set(2048, 65562 / 16, 0x2, 0);
    rxq.dtable[1].set(2048 + (65562 / 16), 65562 / 16, 0x2, 0);
    rxq.dtable[2].set(2048 + (65562 / 16) * 2, 65562 / 16, 0x2, 0);
    rxq.dtable[3].set(2048 + (65562 / 16) * 3, 65562 / 16, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.ring[1].set(1);
    rxq.avail.ring[2].set(2);
    rxq.avail.ring[3].set(3);
    rxq.avail.idx.set(4);

    c.bench_function("queue_pop_4", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            while let Some(desc) = queue.pop(&mem) {
                std::hint::black_box(desc);
            }
        })
    });

    rxq.dtable[0].set(2048, 65562 / 16, 0x2, 0);
    rxq.dtable[1].set(2048 + (65562 / 16), 65562 / 16, 0x2, 0);
    rxq.dtable[2].set(2048 + (65562 / 16) * 2, 65562 / 16, 0x2, 0);
    rxq.dtable[3].set(2048 + (65562 / 16) * 3, 65562 / 16, 0x2, 0);
    rxq.dtable[4].set(2048 + (65562 / 16) * 4, 65562 / 16, 0x2, 0);
    rxq.dtable[5].set(2048 + (65562 / 16) * 5, 65562 / 16, 0x2, 0);
    rxq.dtable[6].set(2048 + (65562 / 16) * 6, 65562 / 16, 0x2, 0);
    rxq.dtable[7].set(2048 + (65562 / 16) * 7, 65562 / 16, 0x2, 0);
    rxq.dtable[8].set(2048 + (65562 / 16) * 8, 65562 / 16, 0x2, 0);
    rxq.dtable[9].set(2048 + (65562 / 16) * 9, 65562 / 16, 0x2, 0);
    rxq.dtable[10].set(2048 + (65562 / 16) * 10, 65562 / 16, 0x2, 0);
    rxq.dtable[11].set(2048 + (65562 / 16) * 11, 65562 / 16, 0x2, 0);
    rxq.dtable[12].set(2048 + (65562 / 16) * 12, 65562 / 16, 0x2, 0);
    rxq.dtable[13].set(2048 + (65562 / 16) * 13, 65562 / 16, 0x2, 0);
    rxq.dtable[14].set(2048 + (65562 / 16) * 14, 65562 / 16, 0x2, 0);
    rxq.dtable[15].set(2048 + (65562 / 16) * 15, 65562 / 16, 0x2, 0);
    rxq.avail.ring[0].set(0);
    rxq.avail.ring[1].set(1);
    rxq.avail.ring[2].set(2);
    rxq.avail.ring[3].set(3);
    rxq.avail.ring[4].set(4);
    rxq.avail.ring[5].set(5);
    rxq.avail.ring[6].set(6);
    rxq.avail.ring[7].set(7);
    rxq.avail.ring[8].set(8);
    rxq.avail.ring[9].set(9);
    rxq.avail.ring[10].set(10);
    rxq.avail.ring[11].set(11);
    rxq.avail.ring[12].set(12);
    rxq.avail.ring[13].set(13);
    rxq.avail.ring[14].set(14);
    rxq.avail.ring[15].set(15);
    rxq.avail.idx.set(16);

    c.bench_function("queue_pop_16", |b| {
        b.iter(|| {
            queue.next_avail = Wrapping(0);
            while let Some(desc) = queue.pop(&mem) {
                std::hint::black_box(desc);
            }
        })
    });

    c.bench_function("queue_add_used_1", |b| {
        b.iter(|| {
            queue.num_added = Wrapping(0);
            queue.next_used = Wrapping(0);
            for i in 0_u16..1_u16 {
                let index = std::hint::black_box(i);
                let len = std::hint::black_box(i + 1);
                // SAFETY: will never panic.
                unsafe { queue.add_used(index as u16, len as u32).unwrap_unchecked() };
            }
        })
    });

    c.bench_function("queue_add_used_16", |b| {
        b.iter(|| {
            queue.num_added = Wrapping(0);
            queue.next_used = Wrapping(0);
            for i in 0_u16..16_u16 {
                let index = std::hint::black_box(i);
                let len = std::hint::black_box(i + 1);
                // SAFETY: will never panic.
                unsafe { queue.add_used(index as u16, len as u32).unwrap_unchecked() };
            }
        })
    });

    c.bench_function("queue_add_used_256", |b| {
        b.iter(|| {
            queue.num_added = Wrapping(0);
            queue.next_used = Wrapping(0);
            for i in 0_u16..256_u16 {
                let index = std::hint::black_box(i);
                let len = std::hint::black_box(i + 1);
                // SAFETY: will never panic.
                unsafe { queue.add_used(index as u16, len as u32).unwrap_unchecked() };
            }
        })
    });
}

criterion_group! {
    name = queue_benches;
    config = Criterion::default().sample_size(200).noise_threshold(0.05);
    targets = queue_benchmark
}

criterion_main! {
    queue_benches
}
