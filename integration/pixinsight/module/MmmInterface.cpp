// MmmInterface.cpp -- MergeMosaic ProcessInterface implementation (Task 3).
//
// Builds the real control tree (deferred-init GUIData, per PCL_API_REFERENCE.md
// section 4) and wires every control to the interface's private working
// MmmBlendInstance (m_instance) using the direct OnXxx(handler, receiver)
// idiom -- there is no __CLASS_HANDLER macro in PCL. NewProcess() hands out
// copies of m_instance; ImportProcess() adopts a foreign instance's values
// into m_instance and refreshes the controls.
//
// Mutual exclusion of the Views/Files input (spec section 10.1): the
// interface never lets both p_viewIds and p_filePaths be non-empty at once.
// Switching the toggle, or adding to one side, clears the other side's array
// on the working instance; UpdateControls() re-syncs both radio buttons and
// both TreeBoxes' enabled state unconditionally, so the displayed state can
// never drift from the interface's own model even if the platform's native
// RadioButton exclusivity behaves differently than expected (unconfirmed
// against the reference doc -- see task-3-report.md).

#include "MmmInterface.h"
#include "MmmExecution.h"
#include "MmmIcon.h"
#include "MmmVersion.h"

#include <pcl/Array.h>
#include <pcl/Cursor.h>
#include <pcl/ExternalProcess.h>
#include <pcl/FileDialog.h>
#include <pcl/Graphics.h>
#include <pcl/MultiViewSelectionDialog.h>
#include <pcl/StringList.h>
#include <pcl/View.h>

namespace pcl
{

// ----------------------------------------------------------------------------

MmmBlendInterface* TheMmmBlendInterface = nullptr;

// ----------------------------------------------------------------------------

MmmBlendInterface::MmmBlendInterface()
   : m_instance( TheMmmBlendProcess )
{
   // Constructing a ProcessInterface self-registers it under pcl::Module.
   TheMmmBlendInterface = this;
}

MmmBlendInterface::~MmmBlendInterface()
{
   delete GUI;
}

IsoString MmmBlendInterface::Id() const
{
   return "MegaMergeMosaic";
}

MetaProcess* MmmBlendInterface::Process() const
{
   return TheMmmBlendProcess;
}

InterfaceFeatures MmmBlendInterface::Features() const
{
   // Static interface that executes in the global context.
   return InterfaceFeature::DefaultGlobal;
}

ProcessImplementation* MmmBlendInterface::NewProcess() const
{
   return new MmmBlendInstance( m_instance );
}

bool MmmBlendInterface::ImportProcess( const ProcessImplementation& p )
{
   const MmmBlendInstance* instance = dynamic_cast<const MmmBlendInstance*>( &p );
   if ( instance == nullptr )
      return false;

   m_instance.Assign( *instance );

   // Re-derive the transient Views-vs-Files toggle from which side actually
   // carries data (there is no explicit mode member on the instance -- see
   // spec 10.1). Default to Views mode if both sides happen to be empty.
   m_viewsMode = !m_instance.p_viewIds.IsEmpty() || m_instance.p_filePaths.IsEmpty();

   // Nothing in Assign()/the copy constructor prevents a foreign (legacy or
   // scripted) instance from carrying BOTH arrays populated at once. Enforce
   // the same single-input-type invariant here that every other mutation
   // point (e_ModeClick, e_AddViewsClick, e_AddFilesClick) enforces: clear
   // whichever side m_viewsMode did NOT select.
   if ( m_viewsMode )
      m_instance.p_filePaths.Clear();
   else
      m_instance.p_viewIds.Clear();

   if ( GUI != nullptr )
      UpdateControls();

   return true;
}

bool MmmBlendInterface::Launch( const MetaProcess&, const ProcessImplementation*,
                                 bool& dynamic, unsigned& /*flags*/ )
{
   // Deferred initialization: build the control tree only the first time the
   // interface is actually shown. Per the ProcessInterface::Launch() doc, the
   // core calls ValidateProcess()/ImportProcess() itself right after a
   // successful Launch() from an existing instance, so this function does not
   // need to import `instance` explicitly.
   if ( GUI == nullptr )
   {
      GUI = new GUIData( *this );
      SetWindowTitle( "Mega Merge Mosaic" );
      UpdateControls();
   }

   dynamic = false;
   return true;
}

IsoString MmmBlendInterface::IconImageSVG() const
{
   return MMM_PROCESS_ICON_SVG;
}

// ----------------------------------------------------------------------------
// GUIData -- control tree construction + event wiring.
// ----------------------------------------------------------------------------

MmmBlendInterface::GUIData::GUIData( MmmBlendInterface& w )
{
   //
   // Header notice: chevron logo + title / tagline / copyright, in the style
   // of the MosaicByCoordinates script header.
   //
   Logo_Bitmap = Bitmap( MMM_CHEVRON_SVG, sizeof( MMM_CHEVRON_SVG ) - 1, "SVG" );

   Logo_Control.SetScaledFixedSize( 40, 40 );
   Logo_Control.OnPaint( (Control::paint_event_handler)&MmmBlendInterface::e_LogoPaint, w );

   Title_Label.SetText( "Mega Merge Mosaic version " MMM_VERSION_STRING );
   Title_Label.SetStyleSheet( w.ScaledStyleSheet( "QLabel { font-weight: bold; }" ) );

   Tagline_Label.SetText( "Big mosaics, no big deal." );

   Copyright_Label.SetText( "Copyright (c) 2026 Astrometrical | astrometrical.com" );
   Copyright_Label.SetToolTip( "<p>Visit https://astrometrical.com</p>" );
   Copyright_Label.SetCursor( StdCursor::PointingHand );
   Copyright_Label.OnMouseRelease( (Control::mouse_button_event_handler)&MmmBlendInterface::e_NoticeMouseRelease, w );

   NoticeText_Sizer.SetSpacing( 2 );
   NoticeText_Sizer.Add( Title_Label );
   NoticeText_Sizer.Add( Tagline_Label );
   NoticeText_Sizer.Add( Copyright_Label );

   Notice_Sizer.SetMargin( 6 );
   Notice_Sizer.SetSpacing( 8 );
   Notice_Sizer.Add( Logo_Control );
   Notice_Sizer.Add( NoticeText_Sizer, 100 );
   Notice_Control.SetSizer( Notice_Sizer );

   //
   // Target Frames section.
   //
   ViewsMode_RadioButton.SetText( "Views" );
   ViewsMode_RadioButton.SetChecked();
   ViewsMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   FilesMode_RadioButton.SetText( "Files" );
   FilesMode_RadioButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_ModeClick, w );

   InputMode_Sizer.SetSpacing( 8 );
   InputMode_Sizer.Add( ViewsMode_RadioButton );
   InputMode_Sizer.Add( FilesMode_RadioButton );
   InputMode_Sizer.AddStretch();

   Views_TreeBox.SetNumberOfColumns( 1 );
   Views_TreeBox.SetHeaderText( 0, "View Id" );
   Views_TreeBox.EnableMultipleSelections();
   Views_TreeBox.SetScaledMinSize( 400, 120 );

   AddViews_PushButton.SetText( "Add Views..." );
   AddViews_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddViewsClick, w );

   RemoveView_PushButton.SetText( "Remove" );
   RemoveView_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveViewClick, w );

   ViewButtons_Sizer.SetSpacing( 6 );
   ViewButtons_Sizer.Add( AddViews_PushButton );
   ViewButtons_Sizer.Add( RemoveView_PushButton );
   ViewButtons_Sizer.AddStretch();

   Files_TreeBox.SetNumberOfColumns( 1 );
   Files_TreeBox.SetHeaderText( 0, "File Path" );
   Files_TreeBox.EnableMultipleSelections();
   Files_TreeBox.SetScaledMinSize( 400, 120 );

   AddFiles_PushButton.SetText( "Add Files..." );
   AddFiles_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_AddFilesClick, w );

   RemoveFile_PushButton.SetText( "Remove" );
   RemoveFile_PushButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_RemoveFileClick, w );

   FileButtons_Sizer.SetSpacing( 6 );
   FileButtons_Sizer.Add( AddFiles_PushButton );
   FileButtons_Sizer.Add( RemoveFile_PushButton );
   FileButtons_Sizer.AddStretch();

   TargetFrames_Sizer.SetSpacing( 4 );
   TargetFrames_Sizer.Add( InputMode_Sizer );
   TargetFrames_Sizer.Add( Views_TreeBox );
   TargetFrames_Sizer.Add( ViewButtons_Sizer );
   TargetFrames_Sizer.Add( Files_TreeBox );
   TargetFrames_Sizer.Add( FileButtons_Sizer );
   TargetFrames_Control.SetSizer( TargetFrames_Sizer );

   TargetFrames_SectionBar.SetTitle( "Target Frames" );
   TargetFrames_SectionBar.SetSection( TargetFrames_Control );
   TargetFrames_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );

   //
   // Parameters section.
   //
   SessionDir_Label.SetText( "Session directory:" );

   SessionDir_Edit.OnEditCompleted( (Edit::edit_event_handler)&MmmBlendInterface::e_SessionDirEditCompleted, w );

   SessionDir_ToolButton.SetIcon( Bitmap( w.ScaledResource( ":/browser/select-file.png" ) ) );
   SessionDir_ToolButton.SetScaledFixedSize( 20, 20 );
   SessionDir_ToolButton.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_SessionDirBrowseClick, w );

   SessionDir_Sizer.SetSpacing( 4 );
   SessionDir_Sizer.Add( SessionDir_Label );
   SessionDir_Sizer.Add( SessionDir_Edit, 100 );
   SessionDir_Sizer.Add( SessionDir_ToolButton );

   //
   // Input override (advanced): Auto / Aligned / Solved.
   // Element order MUST match MmmInputSelectParameter (Auto=0/Aligned=1/Solved=2).
   //
   InputSelect_Label.SetText( "Input:" );

   InputSelect_ComboBox.AddItem( "Auto" );
   InputSelect_ComboBox.AddItem( "Aligned" );
   InputSelect_ComboBox.AddItem( "Solved" );
   InputSelect_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_InputSelectItemSelected, w );

   InputSelect_Sizer.SetSpacing( 6 );
   InputSelect_Sizer.Add( InputSelect_Label );
   InputSelect_Sizer.Add( InputSelect_ComboBox );
   InputSelect_Sizer.AddStretch();

   //
   // Blend parameters.
   //
   BlendMode_Label.SetText( "Blend mode:" );

   // Element order MUST match MmmBlendModeParameter (Feather=0/TwoBand=1/Pyramid=2).
   BlendMode_ComboBox.AddItem( "Feather" );
   BlendMode_ComboBox.AddItem( "TwoBand" );
   BlendMode_ComboBox.AddItem( "Pyramid" );
   BlendMode_ComboBox.OnItemSelected( (ComboBox::item_event_handler)&MmmBlendInterface::e_BlendModeItemSelected, w );

   BlendMode_Sizer.SetSpacing( 6 );
   BlendMode_Sizer.Add( BlendMode_Label );
   BlendMode_Sizer.Add( BlendMode_ComboBox );
   BlendMode_Sizer.AddStretch();

   Feather_NumericControl.label.SetText( "Feather:" );
   Feather_NumericControl.SetInteger();
   Feather_NumericControl.SetRange( 1, 1024 );
   Feather_NumericControl.OnValueUpdated( (NumericEdit::value_event_handler)&MmmBlendInterface::e_FeatherValueUpdated, w );

   SurfaceOrder_Label.SetText( "Surface fit order:" );
   SurfaceOrder_SpinBox.SetRange( 0, 2 );
   SurfaceOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_SurfaceOrderValueUpdated, w );

   SurfaceOrder_Sizer.SetSpacing( 6 );
   SurfaceOrder_Sizer.Add( SurfaceOrder_Label );
   SurfaceOrder_Sizer.Add( SurfaceOrder_SpinBox );
   SurfaceOrder_Sizer.AddStretch();

   BandRows_Label.SetText( "Band rows:" );
   BandRows_SpinBox.SetRange( 1, 65536 );
   BandRows_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_BandRowsValueUpdated, w );

   BandRows_Sizer.SetSpacing( 6 );
   BandRows_Sizer.Add( BandRows_Label );
   BandRows_Sizer.Add( BandRows_SpinBox );
   BandRows_Sizer.AddStretch();

   DefectVeto_CheckBox.SetText( "Cross-panel defect veto" );
   DefectVeto_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_DefectVetoClick, w );

   DefectVeto_Sizer.Add( DefectVeto_CheckBox );
   DefectVeto_Sizer.AddStretch();

   FlattenEnabled_CheckBox.SetText( "Flatten background, order:" );
   FlattenEnabled_CheckBox.OnClick( (Button::click_event_handler)&MmmBlendInterface::e_FlattenEnabledClick, w );

   FlattenOrder_SpinBox.SetRange( 1, 2 );
   FlattenOrder_SpinBox.OnValueUpdated( (SpinBox::value_event_handler)&MmmBlendInterface::e_FlattenOrderValueUpdated, w );

   Flatten_Sizer.SetSpacing( 6 );
   Flatten_Sizer.Add( FlattenEnabled_CheckBox );
   Flatten_Sizer.Add( FlattenOrder_SpinBox );
   Flatten_Sizer.AddStretch();

   Parameters_Sizer.SetSpacing( 4 );
   Parameters_Sizer.Add( SessionDir_Sizer );
   Parameters_Sizer.Add( InputSelect_Sizer );
   Parameters_Sizer.Add( BlendMode_Sizer );
   Parameters_Sizer.Add( Feather_NumericControl );
   Parameters_Sizer.Add( SurfaceOrder_Sizer );
   Parameters_Sizer.Add( BandRows_Sizer );
   Parameters_Sizer.Add( DefectVeto_Sizer );
   Parameters_Sizer.Add( Flatten_Sizer );
   Parameters_Control.SetSizer( Parameters_Sizer );

   Parameters_SectionBar.SetTitle( "Parameters" );
   Parameters_SectionBar.SetSection( Parameters_Control );
   Parameters_SectionBar.OnToggleSection( (SectionBar::section_event_handler)&MmmBlendInterface::e_ToggleSection, w );

   //
   // Top-level layout.
   //
   Global_Sizer.SetMargin( 8 );
   Global_Sizer.SetSpacing( 6 );
   Global_Sizer.Add( Notice_Control );
   Global_Sizer.Add( TargetFrames_SectionBar );
   Global_Sizer.Add( TargetFrames_Control );
   Global_Sizer.Add( Parameters_SectionBar );
   Global_Sizer.Add( Parameters_Control );

   w.SetSizer( Global_Sizer );
   w.EnsureLayoutUpdated();
   w.AdjustToContents();
}

// ----------------------------------------------------------------------------
// Header notice: logo paint + copyright link.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_LogoPaint( Control& sender, const pcl::Rect& )
{
   Graphics g( sender );
   if ( !GUI->Logo_Bitmap.IsNull() )
      g.DrawScaledBitmap( sender.BoundsRect(), GUI->Logo_Bitmap );
}

void MmmBlendInterface::e_NoticeMouseRelease( Control&, const pcl::Point&, int button, unsigned, unsigned )
{
   if ( button != MouseButton::Left )
      return;
   try
   {
#ifdef _WIN32
      ExternalProcess::StartProgram( "cmd.exe", StringList() << "/c" << "start" << "https://astrometrical.com" );
#elif defined( __APPLE__ )
      ExternalProcess::StartProgram( "open", StringList() << "https://astrometrical.com" );
#else
      ExternalProcess::StartProgram( "xdg-open", StringList() << "https://astrometrical.com" );
#endif
   }
   catch ( ... )
   {
      // Non-fatal: the URL is visible in the label text anyway.
   }
}

// ----------------------------------------------------------------------------
// SectionBar toggle: fix tree box heights while animating, restore afterwards,
// and let the window shrink when a section closes.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_ToggleSection( SectionBar&, Control& section, bool start )
{
   if ( start )
   {
      GUI->Views_TreeBox.SetFixedHeight();
      GUI->Files_TreeBox.SetFixedHeight();
   }
   else
   {
      GUI->Views_TreeBox.SetScaledMinHeight( 120 );
      GUI->Views_TreeBox.SetMaxHeight( int_max );
      GUI->Files_TreeBox.SetScaledMinHeight( 120 );
      GUI->Files_TreeBox.SetMaxHeight( int_max );
      if ( GUI->TargetFrames_Control.IsVisible() )
         SetVariableHeight();
      else
         SetFixedHeight();
      AdjustToContents();
   }
}

// ----------------------------------------------------------------------------
// Control <-> instance sync.
// ----------------------------------------------------------------------------

void MmmBlendInterface::UpdateControls()
{
   GUI->ViewsMode_RadioButton.SetChecked( m_viewsMode );
   GUI->FilesMode_RadioButton.SetChecked( !m_viewsMode );
   UpdateInputModeControls();

   PopulateViewsTreeBox();
   PopulateFilesTreeBox();

   GUI->SessionDir_Edit.SetText( m_instance.p_sessionDir );

   GUI->InputSelect_ComboBox.SetCurrentItem( m_instance.p_inputSelect );
   GUI->BlendMode_ComboBox.SetCurrentItem( m_instance.p_blendMode );

   GUI->Feather_NumericControl.SetValue( m_instance.p_feather );
   GUI->SurfaceOrder_SpinBox.SetValue( m_instance.p_surfaceOrder );
   GUI->BandRows_SpinBox.SetValue( m_instance.p_bandRows );

   GUI->DefectVeto_CheckBox.SetChecked( m_instance.p_defectVeto );

   GUI->FlattenEnabled_CheckBox.SetChecked( m_instance.p_flattenEnabled );
   GUI->FlattenOrder_SpinBox.SetValue( m_instance.p_flatten );
   UpdateFlattenControls();
}

void MmmBlendInterface::UpdateInputModeControls()
{
   // Show only the active side's list; hiding (not disabling) matches the
   // reference tools and keeps the window compact.
   GUI->Views_TreeBox.SetVisible( m_viewsMode );
   GUI->AddViews_PushButton.SetVisible( m_viewsMode );
   GUI->RemoveView_PushButton.SetVisible( m_viewsMode );

   GUI->Files_TreeBox.SetVisible( !m_viewsMode );
   GUI->AddFiles_PushButton.SetVisible( !m_viewsMode );
   GUI->RemoveFile_PushButton.SetVisible( !m_viewsMode );

   EnsureLayoutUpdated();
   AdjustToContents();
}

void MmmBlendInterface::UpdateFlattenControls()
{
   GUI->FlattenOrder_SpinBox.Enable( m_instance.p_flattenEnabled );
}

void MmmBlendInterface::PopulateViewsTreeBox()
{
   GUI->Views_TreeBox.Clear();
   for ( const String& id : m_instance.p_viewIds )
   {
      TreeBox::Node* node = new TreeBox::Node( GUI->Views_TreeBox );
      node->SetText( 0, id );
   }
}

void MmmBlendInterface::PopulateFilesTreeBox()
{
   GUI->Files_TreeBox.Clear();
   for ( const String& path : m_instance.p_filePaths )
   {
      TreeBox::Node* node = new TreeBox::Node( GUI->Files_TreeBox );
      node->SetText( 0, path );
   }
}

// ----------------------------------------------------------------------------
// Event handlers.
// ----------------------------------------------------------------------------

void MmmBlendInterface::e_ModeClick( Button& sender, bool checked )
{
   // Only react to the button that has just turned ON; this keeps behavior
   // correct whether or not the platform's RadioButton grouping also fires a
   // (checked=false) event on the sibling button.
   if ( !checked )
      return;

   if ( &sender == &GUI->ViewsMode_RadioButton )
   {
      m_viewsMode = true;
      m_instance.p_filePaths.Clear();
   }
   else if ( &sender == &GUI->FilesMode_RadioButton )
   {
      m_viewsMode = false;
      m_instance.p_viewIds.Clear();
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddViewsClick( Button&, bool )
{
   MultiViewSelectionDialog d;
   if ( d.Execute() == StdDialogCode::Ok )
   {
      for ( const View& v : d.Views() )
      {
         String id( v.FullId().UTF8ToUTF16() );
         bool exists = false;
         for ( const String& s : m_instance.p_viewIds )
            if ( s == id )
            {
               exists = true;
               break;
            }
         if ( !exists )
            m_instance.p_viewIds.Add( id );
      }

      // Adding views implies Views mode (mutual exclusion, spec 10.1).
      m_instance.p_filePaths.Clear();
      m_viewsMode = true;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveViewClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Views_TreeBox.SelectedNodes();

   Array<int> rows;
   for ( TreeBox::Node* node : selected )
      rows.Add( GUI->Views_TreeBox.ChildIndex( node ) );
   rows.Sort();

   // Remove from the highest index down so earlier indices stay valid, and
   // keep p_viewIds in lockstep with the TreeBox's top-level row order.
   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_viewIds.Length() )
         m_instance.p_viewIds.Remove( m_instance.p_viewIds.Begin() + row );
   }

   UpdateControls();
}

void MmmBlendInterface::e_AddFilesClick( Button&, bool )
{
   OpenFileDialog d;
   d.LoadImageFilters();
   d.EnableMultipleSelections();
   if ( d.Execute() )
   {
      for ( const String& f : d.FileNames() )
      {
         bool exists = false;
         for ( const String& s : m_instance.p_filePaths )
            if ( s == f )
            {
               exists = true;
               break;
            }
         if ( !exists )
            m_instance.p_filePaths.Add( f );
      }

      // Adding files implies Files mode (mutual exclusion, spec 10.1).
      m_instance.p_viewIds.Clear();
      m_viewsMode = false;

      UpdateControls();
   }
}

void MmmBlendInterface::e_RemoveFileClick( Button&, bool )
{
   IndirectArray<TreeBox::Node> selected = GUI->Files_TreeBox.SelectedNodes();

   Array<int> rows;
   for ( TreeBox::Node* node : selected )
      rows.Add( GUI->Files_TreeBox.ChildIndex( node ) );
   rows.Sort();

   for ( int i = int( rows.Length() ) - 1; i >= 0; --i )
   {
      int row = rows[i];
      if ( row >= 0 && size_type( row ) < m_instance.p_filePaths.Length() )
         m_instance.p_filePaths.Remove( m_instance.p_filePaths.Begin() + row );
   }

   UpdateControls();
}

void MmmBlendInterface::e_SessionDirEditCompleted( Edit& sender )
{
   m_instance.p_sessionDir = sender.Text();
}

void MmmBlendInterface::e_SessionDirBrowseClick( Button&, bool )
{
   GetDirectoryDialog d;
   d.SetCaption( "Mega Merge Mosaic: Select Session Directory" );
   if ( d.Execute() )
   {
      m_instance.p_sessionDir = d.Directory();
      UpdateControls();
   }
}

void MmmBlendInterface::e_InputSelectItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_inputSelect = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_BlendModeItemSelected( ComboBox&, int itemIndex )
{
   m_instance.p_blendMode = pcl_enum( itemIndex );
}

void MmmBlendInterface::e_FeatherValueUpdated( NumericEdit&, double value )
{
   m_instance.p_feather = int32( value );
}

void MmmBlendInterface::e_SurfaceOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_surfaceOrder = int32( value );
}

void MmmBlendInterface::e_BandRowsValueUpdated( SpinBox&, int value )
{
   m_instance.p_bandRows = int32( value );
}

void MmmBlendInterface::e_DefectVetoClick( Button&, bool checked )
{
   m_instance.p_defectVeto = checked;
}

void MmmBlendInterface::e_FlattenEnabledClick( Button&, bool checked )
{
   m_instance.p_flattenEnabled = checked;
   UpdateFlattenControls();
}

void MmmBlendInterface::e_FlattenOrderValueUpdated( SpinBox&, int value )
{
   m_instance.p_flatten = int32( value );
}

// ----------------------------------------------------------------------------

} // namespace pcl
