#!/bin/bash

$@ 2> >(ts 1>&2)

