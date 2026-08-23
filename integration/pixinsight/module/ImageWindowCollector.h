// ImageWindowCollector.h -- mmm::OutputCollector -> PCL ImageWindow adapter (Task 4).
//
// Bridges the PCL-free host transport library's mmm::OutputCollector push
// interface to a PixInsight output window. The IPC host calls begin() once with
// the canvas geometry, then band() per blended band streamed back from the
// worker; this class materializes a new Float32 ImageWindow and writes each
// band into it. The finished window is exposed (Window()) for the caller
// (ExecuteGlobal, Task 5) to Show(); it is NOT shown here.
//
// PCL signatures verified against /opt/PixInsight/include/pcl/{ImageWindow,View,
// Image}.h (PCL_API_REFERENCE.md section 7).

#ifndef __MmmImageWindowCollector_h
#define __MmmImageWindowCollector_h

#include <pcl/ImageWindow.h>
#include <pcl/View.h>

#include "mmm_host.h"

namespace pcl
{

/*!
 * \class ViewWriteLockGuard
 * \brief RAII read+write lock on a View for the duration of a pixel write.
 *
 * Takes the full View::Lock() -- the documented lock for code that MODIFIES
 * a view's image. (View::LockForWrite() is the reader-side lock: it only
 * keeps OTHER writers out while explicitly permitting concurrent core reads,
 * and PCL's own View.h marks its UnlockForWrite() partner "undocumented
 * (i.e., harmful)".) No GUI notifications are sent (notify = false), and the
 * matching Unlock() is guaranteed on every exit path: an exception thrown
 * while the view is locked must never leak a permanently locked -- hence
 * unusable -- window into the core.
 */
class ViewWriteLockGuard
{
public:

   /*! Locks \a view for read+write, without GUI notifications. */
   explicit ViewWriteLockGuard( View& view )
      : m_view( view )
   {
      m_view.Lock( false /*notify*/ );
   }

   /*! Releases the lock; an unlock failure is swallowed (never throws). */
   ~ViewWriteLockGuard() noexcept
   {
      try
      {
         m_view.Unlock( false /*notify*/ );
      }
      catch ( ... )
      {
      }
   }

   ViewWriteLockGuard( const ViewWriteLockGuard& ) = delete;
   ViewWriteLockGuard& operator=( const ViewWriteLockGuard& ) = delete;

private:

   View& m_view;
};

/*!
 * \class ImageWindowCollector
 * \brief Collects blended output bands into a new PixInsight ImageWindow.
 *
 * begin(w,h,ch) creates one hidden Float32 ImageWindow (color iff ch >= 3);
 * band(y0,rows,planar,width,ch) writes the planar band into the window's main
 * view at absolute canvas row y0. The planar decode is the exact inverse of
 * ViewPanelSource's encode -- channel c of the band lives at planar + c*rows*
 * width, and within a channel band rows are contiguous, so each channel is one
 * memcpy into ScanLine(y0,c) (PROTOCOL.md section 7).
 *
 * Locking: the freshly created window is fully locked around each band
 * (ViewWriteLockGuard: View::Lock()/Unlock(), no GUI notify) and left
 * unlocked afterwards, so the caller can Show() it directly. The window is
 * never shown by this class.
 */
class ImageWindowCollector : public mmm::OutputCollector
{
public:

   ImageWindowCollector() = default;

   /*!
    * Destructor. Explicitly noexcept: the pinned PCL (2.8.x, PixInsight
    * 1.9.0) declares UIObject::~UIObject() noexcept(false), which would
    * otherwise loosen the implicit spec below the noexcept ~OutputCollector()
    * base and fail to compile; 2.10.x headers made it noexcept anyway.
    */
   ~ImageWindowCollector() noexcept override = default;

   /*!
    * Creates the output window for a \a w x \a h canvas with \a ch channels
    * (Float32, color iff ch >= 3). Called exactly once, before any band().
    */
   void begin( uint64_t w, uint64_t h, uint64_t ch ) override;

   /*!
    * Writes \a rows rows (\a ch channels, \a width wide) from the planar band
    * \a planar into the output window starting at canvas row \a y0.
    */
   void band( uint64_t y0, uint64_t rows, const float* planar, uint64_t width,
              uint64_t ch ) override;

   /*!
    * The output window built by begin()/band(). Valid (non-null) only after
    * begin() has run. The caller owns showing/closing it.
    */
   ImageWindow& Window() { return m_window; }

private:

   ImageWindow m_window = ImageWindow::Null();
   uint64_t    m_width  = 0;
   uint64_t    m_height = 0;
   uint64_t    m_channels = 0;
};

// ----------------------------------------------------------------------------

} // namespace pcl

#endif   // __MmmImageWindowCollector_h
