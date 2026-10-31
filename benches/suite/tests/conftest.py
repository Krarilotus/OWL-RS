"""pytest: the suite's tests outside the repository a calling git names, run in a scratch
directory (isolation.py)."""
import os

import pytest

import isolation  # removes git's repository-locating variables on import


@pytest.fixture(autouse=True, scope="session")
def _scratch_directory():
    os.chdir(isolation.SCRATCH)
    yield
    os.chdir(isolation.START)
