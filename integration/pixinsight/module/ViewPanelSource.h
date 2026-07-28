// ViewPanelSource.h -- PCL View -> mmm::PanelSource adapter (Task 4).
//
// Bridges a set of PixInsight views (the selected mosaic panels, in panel-id
// order) to the PCL-free host transport library's mmm::PanelSource pull
// interface. The IPC host (Plan 2a) calls fill_band() whenever the worker asks
// for rows of a panel; this class copies those rows out of the live core-side
// image, converting to Float32 in the planar layout the shared-memory protocol
// requires (PROTOCOL.md section 7).
//
// PCL signatures verified against /opt/PixInsight/include/pcl/{View,ImageVariant,
// Image,PixelTraits}.h (PCL_API_REFERENCE.md section 5).
//
// Locking contract: View::Image() requires the view be locked for read before
// any pixel access (View.h:487-490 -- a hard requirement, PixInsight is
// multithreaded). This adapter does NOT lock/unlock; the caller (ExecuteGlobal,
// Task 5) owns view lifetime and must Lock() every view for read before the
// IPC run and Unlock() after. The views must outlive this object.

#ifndef __MmmViewPanelSource_h
#define __MmmViewPanelSource_h

#include <pcl/Array.h>
#include <pcl/View.h>

#include "mmm_host.h"

namespace pcl
{

/*!
 * \class ViewPanelSource
 * \brief Serves panel pixel bands from PixInsight views over the mmm IPC host.
 *
 * Constructed from the ordered array of selected panel views; panel_id indexes
 * into that array (matching PanelDesc.panel_id / BandRequest.panel_id). Each
 * fill_band() copies rows [y0,y1) of every channel of one view into the host's
 * planar, native-endian Float32 slot buffer:
 *
 *     index(c, r, x) = c*rows*width + r*width + x,   rows = y1 - y0
 *
 * which matches mmm::PanelSource's contract (mmm_host.h) and PROTOCOL.md
 * section 7 exactly.
 *
 * Sample-format handling (branch on ImageVariant::IsFloatSample()/
 * BitsPerSample()): a Float32 view (pcl::Image) is copied one memcpy per
 * channel -- ScanLine(y0,c) already points at the first of rows*width
 * contiguous samples of that channel plane. Float64 and 8/16/32-bit integer
 * views are converted element-wise to normalized [0,1] Float32 via the pixel
 * traits' FromSample() (the same normalization PixInsight applies). Complex
 * views are rejected. The const pixel-access overloads are used throughout so a
 * shared multi-GB image is never duplicated by copy-on-write.
 */
class ViewPanelSource : public mmm::PanelSource
{
public:

   /*!
    * Constructs a source over \a views, in panel-id order. The views are
    * aliased (PCL views are lightweight managed handles); the caller retains
    * ownership and is responsible for locking each for read before use and
    * unlocking after (see the locking contract above).
    */
   explicit ViewPanelSource( const Array<View>& views );

   /*!
    * Fills rows [\a y0, \a y1) of every channel of panel \a panel_id into
    * \a dst in the host's planar Float32 layout. Returns false (leaving the
    * host to signal a failed fill) if \a panel_id is out of range, the row
    * range is invalid or exceeds the panel height, the view carries no image,
    * or the image is of an unsupported (complex) sample format.
    */
   bool fill_band( uint32_t panel_id, uint64_t y0, uint64_t y1, float* dst ) override;

private:

   Array<View> m_views;
};

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmViewPanelSource_h
