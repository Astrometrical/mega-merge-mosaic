// MmmExecution.h -- ExecuteGlobal orchestration for the MegaMergeMosaic blend (Task 5).
//
// run_blend() implements spec section 10.1's execution flow: validate the
// selection, resolve the effective JobMode, build the ordered PanelDescs and
// the {"Init":{...}} wire object (PROTOCOL.md section 6), size the shared-memory
// slots (solved mode probes the worker for the reprojected frame width), then
// drive the PCL-free host transport library (mmm::Host, Plan 2a) over shared
// memory and assemble ONE new ImageWindow from the streamed output bands.
//
// Fault isolation (spec section 9): any host/worker failure surfaces as a clean
// pcl::Error and NO partial output window is shown; the source views are left
// intact. Cancellation is Console-only (Console's Pause/Abort button) and
// handled inside ConsoleProgress::on_progress via AbortRequested().

#ifndef __MmmExecution_h
#define __MmmExecution_h

#include <string>
#include <vector>

#include "mmm_host.h"

namespace pcl
{

class MmmBlendInstance;

/*!
 * \brief FILTER names of panel files, read header-only by the worker's
 * --probe-panels (pumped so the GUI stays responsive). Returns the probe
 * result; `panels[i].filter` is empty when a file has no FILTER.
 */
mmm::PanelProbeResult probe_filter_names( const std::vector<std::string>& pathsUtf8 );

/*!
 * \brief Runs one blend job described by \a instance to completion.
 *
 * Enumerates the selected views (or files), builds the worker Init job, drives
 * mmm::Host over shared memory, and shows the resulting ImageWindow on success.
 * Throws pcl::Error (or lets a pcl::Error/pcl::ProcessAborted propagate) on any
 * validation failure, worker fault, or cancellation; on every error path no
 * output window is presented. Intended to be called from
 * MmmBlendInstance::ExecuteGlobal().
 */
void run_blend( MmmBlendInstance& instance );

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmExecution_h
